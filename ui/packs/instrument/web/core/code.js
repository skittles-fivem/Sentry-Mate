// A two-step code field takes either six digits from the authenticator app or a recovery code
// (xxxx-xxxx-xxxx-xxxx, 19 characters, typed with or without dashes or spaces, in any case).
//
//   attrs: the input's attributes: room for a recovery code, no digits-only pattern, no letter forced to capitals
//   codeValue(text): what is sent. Six digits typed in groups ("123 456") lose their spaces; anything else (a recovery
//                    code) goes as typed, trimmed: the server reads dashes, spaces and case itself.

export const CODE_HINT = 'Six digits from the app, which change every 30 seconds, or one of your recovery codes.';

export const codeAttrs = () => ({ autocomplete: 'one-time-code', inputmode: 'text', autocapitalize: 'none', spellcheck: 'false', maxlength: '24' });

export function codeValue(text) {
  const t = String(text ?? '').trim();
  return /^[\d\s]*$/.test(t) ? t.replace(/\s+/g, '') : t;
}
