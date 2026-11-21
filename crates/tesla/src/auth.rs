//! Signing in to a Tesla account (OAuth 2 with PKCE, client `ownerapi`) and keeping the tokens fresh.
//!
//! The person signs in on Tesla's own page in their own browser: Sentry Mate never sees the password, the
//! two-step code or the captcha. Tesla then sends the browser to the callback address, which a browser cannot
//! open; the person copies that address and pastes it back, and the server swaps the code in it for tokens.
//! Or the person pastes a refresh token another app made.
//!
//! Refresh tokens are single use: every refresh answers a new one, which the caller must store before it uses the
//! new access token. Nothing here retries against Tesla's sign-in on its own.

use crate::hosts::Endpoints;
use crate::http::{Body, CAP_TOKEN, Http, HttpError};
use crate::secret::Secret;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub const CLIENT_ID: &str = "ownerapi";
pub const SCOPE: &str = "openid email offline_access";
/// How long a started sign-in waits for its pasted address.
pub const SIGN_IN_TTL_MS: u64 = 10 * 60_000;

/// RFC 3986 unreserved characters stay as they are; everything else is percent-encoded.
const ENCODE: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

fn enc(s: &str) -> String {
    utf8_percent_encode(s, ENCODE).to_string()
}

/// The callback address Tesla sends the browser to. Tesla registered `tesla://auth/callback` for `ownerapi` in
/// June 2026 and stopped taking the older `https://auth.tesla.com/void/callback`; the older one stays as a choice
/// in case Tesla moves again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Redirect {
    #[default]
    App,
    Void,
}

impl Redirect {
    pub fn uri(self, e: &Endpoints) -> String {
        match self {
            Redirect::App => "tesla://auth/callback".to_string(),
            Redirect::Void => format!("{}/void/callback", e.auth.https()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// The pasted text is not Tesla's callback address with a code (the reason in plain words).
    #[error("{0}")]
    BadPaste(&'static str),
    /// The pasted address belongs to another sign-in.
    #[error("the pasted address is from another sign-in; start again")]
    WrongState,
    /// The sign-in was started too long ago, or was already used.
    #[error("this sign-in has expired; start again")]
    Expired,
    /// Tesla refused the code or the refresh token (expired, used, replaced, or the password was changed).
    #[error("Tesla refused the sign-in; sign in again")]
    Refused,
    #[error("Tesla asked us to wait")]
    RateLimited { retry_after_s: Option<u64> },
    #[error("Tesla could not be reached: {0}")]
    Unreachable(HttpError),
    #[error("Tesla answered {status}")]
    Tesla { status: u16 },
    #[error("Tesla's answer could not be read")]
    BadAnswer,
}

impl AuthError {
    pub fn code(&self) -> &'static str {
        match self {
            AuthError::BadPaste(_) => "bad_paste",
            AuthError::WrongState => "wrong_state",
            AuthError::Expired => "expired",
            AuthError::Refused => "tesla_refused",
            AuthError::RateLimited { .. } => "rate_limited",
            AuthError::Unreachable(_) => "tesla_unreachable",
            AuthError::Tesla { .. } => "tesla_error",
            AuthError::BadAnswer => "bad_answer",
        }
    }
}

/// What Tesla's token endpoint gave.
#[derive(Debug, Clone)]
pub struct Tokens {
    pub access: Secret,
    /// The new refresh token. Tesla may leave it out of a refresh answer; then the old one stays.
    pub refresh: Option<Secret>,
    pub expires_in_s: u64,
}

fn random_text(bytes: usize) -> Result<String, AuthError> {
    let mut b = Zeroizing::new(vec![0u8; bytes]);
    getrandom::fill(&mut b).map_err(|_| AuthError::BadAnswer)?;
    Ok(URL_SAFE_NO_PAD.encode(&*b))
}

/// `base64url(SHA-256(verifier))` without padding (RFC 7636 `S256`).
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A sign-in that was started and waits for the pasted address. The verifier never leaves the server.
pub struct SignIn {
    verifier: Secret,
    state: String,
    pub redirect: Redirect,
    pub started_ms: u64,
    used: bool,
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignIn").field("redirect", &self.redirect).field("started_ms", &self.started_ms).finish()
    }
}

impl SignIn {
    /// A new sign-in and the address of Tesla's sign-in page for it.
    pub fn start(e: &Endpoints, redirect: Redirect, now_ms: u64) -> Result<(SignIn, String), AuthError> {
        // 64 random bytes: 86 characters, inside RFC 7636's 43 to 128.
        let verifier = Secret::new(random_text(64)?);
        let state = random_text(24)?;
        let url = format!(
            "{}/oauth2/v3/authorize?client_id={CLIENT_ID}&code_challenge={}&code_challenge_method=S256&\
             redirect_uri={}&response_type=code&scope={}&state={}",
            e.auth.https(),
            challenge(verifier.expose()),
            enc(&redirect.uri(e)),
            enc(SCOPE),
            enc(&state),
        );
        Ok((SignIn { verifier, state, redirect, started_ms: now_ms, used: false }, url))
    }

    pub fn expires_ms(&self) -> u64 {
        self.started_ms + SIGN_IN_TTL_MS
    }

    /// The code in the pasted callback address. Checks it is this sign-in's callback (same address, same
    /// `state`, Tesla's issuer when it is named).
    pub fn code_from(&self, e: &Endpoints, pasted: &str, now_ms: u64) -> Result<Secret, AuthError> {
        if self.used || now_ms > self.expires_ms() {
            return Err(AuthError::Expired);
        }
        let pasted = pasted.trim();
        if pasted.len() > 8192 {
            return Err(AuthError::BadPaste("the pasted text is too long"));
        }
        let Some((base, query)) = pasted.split_once('?') else {
            return Err(AuthError::BadPaste("paste the whole address, with ?code= in it"));
        };
        let base = base.trim_end_matches('/');
        let expected = self.redirect.uri(e);
        if !base.eq_ignore_ascii_case(expected.trim_end_matches('/')) {
            return Err(AuthError::BadPaste("this is not Tesla's sign-in callback address"));
        }
        let query = query.split('#').next().unwrap_or_default();
        let mut code = None;
        let mut state = None;
        let mut issuer = None;
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let v = Zeroizing::new(percent_decode_str(&v.replace('+', " ")).decode_utf8_lossy().into_owned());
            match k {
                "code" => code = Some(v),
                "state" => state = Some(v),
                "issuer" => issuer = Some(v),
                "error" => return Err(AuthError::Refused),
                _ => {}
            }
        }
        if state.as_deref().map(|s| s.as_str()) != Some(self.state.as_str()) {
            return Err(AuthError::WrongState);
        }
        if let Some(i) = issuer
            && i.trim_end_matches('/') != e.issuer()
        {
            return Err(AuthError::BadPaste("the address names another sign-in server"));
        }
        match code {
            Some(c) if !c.is_empty() && c.len() <= 4096 => Ok(Secret::from(c)),
            _ => Err(AuthError::BadPaste("the address has no code in it")),
        }
    }

    /// Swaps the pasted code for tokens. A sign-in is used once, whether Tesla takes the code or not.
    pub async fn finish(&mut self, http: &Http, code: &Secret) -> Result<Tokens, AuthError> {
        if self.used {
            return Err(AuthError::Expired);
        }
        self.used = true;
        let e = http.endpoints();
        let form = Zeroizing::new(format!(
            "grant_type=authorization_code&client_id={CLIENT_ID}&code={}&code_verifier={}&redirect_uri={}",
            enc(code.expose()),
            enc(self.verifier.expose()),
            enc(&self.redirect.uri(e)),
        ));
        token_request(http, &form).await
    }
}

/// Swaps a refresh token for a new access token (and, nearly always, a new refresh token).
pub async fn refresh(http: &Http, refresh_token: &Secret) -> Result<Tokens, AuthError> {
    let form = Zeroizing::new(format!(
        "grant_type=refresh_token&client_id={CLIENT_ID}&refresh_token={}&scope={}",
        enc(refresh_token.expose()),
        enc(SCOPE),
    ));
    token_request(http, &form).await
}

#[derive(Deserialize)]
struct TokenAnswer {
    access_token: Zeroizing<String>,
    #[serde(default)]
    refresh_token: Option<Zeroizing<String>>,
    #[serde(default)]
    expires_in: Option<u64>,
}

async fn token_request(http: &Http, form: &str) -> Result<Tokens, AuthError> {
    let e = http.endpoints().clone();
    let a = http
        .request(&e.auth, true, "/oauth2/v3/token", None, Body::Form(form), CAP_TOKEN)
        .await
        .map_err(AuthError::Unreachable)?;
    match a.status {
        200 => {
            let t: TokenAnswer = serde_json::from_slice(&a.body).map_err(|_| AuthError::BadAnswer)?;
            if t.access_token.is_empty() {
                return Err(AuthError::BadAnswer);
            }
            tracing::info!("tesla tokens refreshed");
            Ok(Tokens {
                access: Secret::from(t.access_token),
                refresh: t.refresh_token.filter(|r| !r.is_empty()).map(Secret::from),
                // An answer without a lifetime: assume five minutes, so the next call refreshes again soon.
                expires_in_s: t.expires_in.unwrap_or(300).clamp(60, 30 * 86_400),
            })
        }
        400 | 401 => Err(AuthError::Refused),
        429 => Err(AuthError::RateLimited { retry_after_s: a.retry_after_s }),
        status => Err(AuthError::Tesla { status }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::hosts::Region;

    #[test]
    fn challenge_is_rfc_7636() {
        // RFC 7636 appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn the_sign_in_address() {
        let e = Endpoints::for_region(Region::Global);
        let (s, url) = SignIn::start(&e, Redirect::App, 1_000).unwrap();
        assert!(url.starts_with("https://auth.tesla.com/oauth2/v3/authorize?client_id=ownerapi&code_challenge="));
        assert!(url.contains("&code_challenge_method=S256&redirect_uri=tesla%3A%2F%2Fauth%2Fcallback&"));
        assert!(url.contains("&scope=openid%20email%20offline_access&state="));
        assert!(!url.contains(s.verifier.expose()));
        assert!((43..=128).contains(&s.verifier.expose().len()));
        assert!(url.contains(&format!("code_challenge={}", challenge(s.verifier.expose()))));
        let (_, cn) = SignIn::start(&Endpoints::for_region(Region::China), Redirect::Void, 0).unwrap();
        assert!(cn.starts_with("https://auth.tesla.cn/oauth2/v3/authorize?"));
        assert!(cn.contains("redirect_uri=https%3A%2F%2Fauth.tesla.cn%2Fvoid%2Fcallback&"));
    }

    #[test]
    fn the_pasted_address_is_checked() {
        let e = Endpoints::for_region(Region::Global);
        let (s, _) = SignIn::start(&e, Redirect::App, 1_000).unwrap();
        let st = enc(&s.state);
        let good = format!(
            "tesla://auth/callback?code=made-up-code&state={st}&issuer=https%3A%2F%2Fauth.tesla.com%2Foauth2%2Fv3"
        );
        assert_eq!(s.code_from(&e, &good, 2_000).unwrap().expose(), "made-up-code");
        assert_eq!(s.code_from(&e, &format!("  {good}\n"), 2_000).unwrap().expose(), "made-up-code");
        assert_eq!(s.code_from(&e, &good, 1_000 + SIGN_IN_TTL_MS + 1), Err(AuthError::Expired));
        let other = "tesla://auth/callback?code=x&state=someone-else";
        assert_eq!(s.code_from(&e, other, 2_000), Err(AuthError::WrongState));
        let host = format!("https://evil.example/callback?code=x&state={st}");
        assert!(matches!(s.code_from(&e, &host, 2_000), Err(AuthError::BadPaste(_))));
        let issuer = format!("tesla://auth/callback?code=x&state={st}&issuer=https%3A%2F%2Fevil.example%2Foauth2%2Fv3");
        assert!(matches!(s.code_from(&e, &issuer, 2_000), Err(AuthError::BadPaste(_))));
        let no_code = format!("tesla://auth/callback?state={st}");
        assert!(matches!(s.code_from(&e, &no_code, 2_000), Err(AuthError::BadPaste(_))));
        assert!(matches!(s.code_from(&e, "made-up-code", 2_000), Err(AuthError::BadPaste(_))));
        let denied = format!("tesla://auth/callback?error=access_denied&state={st}");
        assert_eq!(s.code_from(&e, &denied, 2_000), Err(AuthError::Refused));
        // The older callback is only taken by a sign-in that used it.
        let void = format!("https://auth.tesla.com/void/callback?code=x&state={st}");
        assert!(matches!(s.code_from(&e, &void, 2_000), Err(AuthError::BadPaste(_))));
        let (v, _) = SignIn::start(&e, Redirect::Void, 0).unwrap();
        let void = format!("https://auth.tesla.com/void/callback?code=y&state={}", enc(&v.state));
        assert_eq!(v.code_from(&e, &void, 1).unwrap().expose(), "y");
    }

    #[test]
    fn errors_hold_no_secret() {
        let e = AuthError::Unreachable(HttpError::Timeout);
        assert_eq!(e.to_string(), "Tesla could not be reached: timed out");
        assert_eq!(e.code(), "tesla_unreachable");
    }
}
