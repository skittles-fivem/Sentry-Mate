# Security and privacy

[← Back to the README](../README.md)

- [Security](#security)
- [The Tesla API input](#the-tesla-api-input)
- [No way back without a backup](#no-way-back-without-a-backup)
- [Privacy](#privacy)
- [When a log fails its check](#when-a-log-fails-its-check)
- [A new signing key](#a-new-signing-key)

## Security

- TLS 1.3 only on the upload port; plain HTTP and older TLS are refused. The certificate is self-signed and made on
  first start, its private key sealed in the vault; the app pins it by SHA-256.
- The upload port can't be filled without a token: connections are counted per address (an IPv6 /64 is one), the
  TLS handshake and the headers must come within 10 s each, and a connection that has not shown a valid token is
  closed, oldest first, to make room for a new one, so the CAN Bridge's uploads still get in.
- A log can't unpack to more than it is worth: at most 200 times its packed size (at least 1 MiB), 4 GB of candump
  text, a columns body of 64 MiB and 8 million frames. Real logs unpack to under 10 times their size, so a few
  kilobytes crafted to unpack to gigabytes are cut or refused (a refused log is a hole, never decoded again).
- Pages that decode stored logs while they answer (signals, the battery pages, efficiency) take turns: at most 3
  run at once for the whole server and 4 run or wait per account (`503 busy`, `429 throttled` beyond), a request
  that goes away stops its decoding, and one answer decodes at most 2 GiB of log lines. A watch account can't keep
  every core busy, or starve uploads and the engine.
- Tokens are write-only: they can upload and ask which uploads arrived, nothing else. They are stored only as their
  SHA-256 and tied to one car key on the first upload. That first upload never takes a car key in use already
  (another token is tied to it, or its logs are here), unless the token was made for that key's car (on the car's
  page, or `token new --key`): car keys are in every log name, so they are not secret, and a token handed out for one
  car could otherwise upload into another car's history, block its log names or pair its own signing key with it. Wrong tokens are rate limited per address; a right
  token is never refused because of wrong ones.
- Every log's ECDSA P-256 signature is checked against the key the car key is paired with. A log signed by another
  key is kept and marked failed, and not used until the owner trusts that key (`keys trust`, after a `yes`). Only
  a key that signed some of the car key's logs can be trusted. Deleted, swapped or reordered logs show in the
  chain.
- **Encryption at rest, in Rust.** One random data key seals the contents of everything stored (logs, sidecars,
  uploads still arriving, trust records, tokens, the TLS key) with XChaCha20-Poly1305, in 64 KiB chunks, each file
  with its own key (HKDF-SHA256) and its path bound in, so a file that is changed, cut short, or moved under another
  name doesn't open. Files that grow (uploads still arriving, journals) give every piece its own random nonce, so a
  piece written again after a crash never reuses one, and bind each piece to the one before it, so pieces can't be
  reordered, dropped from the middle or mixed with those of an older copy of the file (an older copy put back whole,
  or pieces missing at the end, can't be told from a crash). The data key is kept only wrapped: by the key file in
  `/keys` (a separate volume, refused inside the data folder) for restarts with nothing to type, and by the recovery
  key through Argon2id at 1 GiB. Keys are wiped from memory when no longer needed and core dumps are off. This
  protects a stolen or copied disk or backup; it can't protect a server taken over while it runs. File and folder
  names, sizes and times are not sealed (see [Privacy](#privacy)).
- Pure Rust where it matters (`unsafe` forbidden in this code, the xz reader included); files from the network are
  read with fixed limits. The image is one static binary on `scratch`.
- **How the image is built.** The workflow that builds and publishes it (`.github/workflows/image.yml`, the only
  one with a token that can push `latest`) runs each GitHub Action at a full commit id, never a tag or branch the
  action's owner could move, and the Dockerfile builds on the Rust image pinned by digest. A taken-over action or
  base image tag upstream can't change what goes into the image. Dependabot proposes new pins as changes to review,
  and a test (`crates/sentry-mate/tests/repo_hygiene.rs`) fails on an unpinned action or image.
- **The container.** The image runs as 1000:1000; the TrueNAS file runs it as TrueNAS's apps user, 568:568. The
  compose and TrueNAS files drop every capability, make the root read-only and set `no-new-privileges`, so it can
  write only to its two mounted folders.
- **Folders it can't keep are refused**, before anything is written: a data or keys folder that is not mounted (it
  would be lost with the container, the key file with it), a keys folder inside the data folder or the same folder
  at both (the key file would be in the data's backups), and a folder it can't write to (most often root's).
- **A new vault is never made over an existing key file.** An empty or wrong data folder next to a key file stops it
  instead, so the key file that opens your data is never replaced.
- Nothing leaves the server unless an admin turns it on. These are all the outgoing connections the server makes,
  and the switch for each (Settings → Integrations lists them, `GET /api/v1/network`):
  - **MQTT**: to the broker an admin sets up, while MQTT is on.
  - **The Tesla API**: to Tesla's own hosts only, once an admin adds a Tesla account. See
    [The Tesla API input](#the-tesla-api-input).
  - **A move**: a new server, during a move you started, connects to the old server's upload port, and nowhere else.

  With none of these turned on, Sentry Mate makes no outgoing connection at all.
- The viewers' browsers load nothing from outside either. Map tiles are offline until an admin picks online tiles in
  Settings; until then the pages' Content-Security-Policy names no outside host and every map draws the track on a
  plain background. Online tiles send each viewer's browser to that one tile server, which then sees the viewer's
  address and the tiles asked for (where the car drove and parked), so it stays off unless the owner chooses it.
- **The move code** ([moving.md](moving.md)) is 125 random bits, works once, for 24 hours, and only from the home
  network unless made with `--anywhere`. Logs name it by a short id. Only the old server shows the code itself,
  when asked (`move code`, or in its log with `SENTRY_MATE_OFFER_MOVE`). The new server proves the code over the TLS
  connection itself, so a fake server or someone in between learns nothing from it. The data key travels last,
  wrapped with a key from the code, inside TLS 1.3. The key file never leaves its folder: the new server writes a
  key file of its own, and the recovery key stays the same. Treat the code like a password until it is used, and
  run `move cancel` on the old server if someone else may have used it.
- **An export file never holds the key file.** It opens only with the recovery key (Argon2id at 1 GiB), so a
  copied export file is useless without it. It still holds everything, sealed as stored: keep it private, and delete
  it once the new server runs.
- A move or an import goes only into a new server: empty data and keys folders, or only what a first start made
  there. A key file that opens other data is never replaced.
- **The audit log names what changed.** Moving a CAN Bridge key between cars (it decides whose history a car's signed
  logs count for), marking it preferred, and making or revoking an upload token are each recorded with the key or
  token id and the car (both cars for a move), so another admin can see which car's history was redirected. A
  change that is not worded on its own is recorded with the ids of its request. A watch account can't fill the
  log: the places preview writes nothing, and the same preferences change within 10 minutes is one entry.
- A watch account's set-up status speaks only of its own cars: it never shows a CAN Bridge key waiting for a car,
  nor counts the tokens made for a new car.
- **Signing in to the web UI.** Passwords are argon2id hashes; failed tries wait longer each time, per address and
  per account (home and outside counted apart); right passwords too are limited (20 sign-ins a minute per account,
  one check at a time per account), so a loop of sign-ins can't keep the few password checks busy for everyone else.
  A password change signs out every other session, even a sign-in that was still being checked with the old
  password at that moment. With the "two-step from outside" rule, a right password of an account without two-step
  sign-in gets, from outside, the answer a wrong one gets, so the internet can't learn that a guess was right.
- **Passwords are the person's choice.** The server refuses only an empty password and one over 1,024 bytes, for
  every account, the first admin included: no length rule, nothing about the account name. The pages check a new
  password first (shorter than 12 characters, close to the account name, one of the most common, repeating itself)
  and, when one of those holds, say why in a pop-up with "Use it anyway" and "Change it". A weak password is still
  hard to use from outside: argon2id makes each guess slow, the throttles above stop a guessing loop after a few
  tries, the web UI is HTTPS only (a password never crosses the network in the clear), and two-step sign-in (or the
  "two-step from outside" rule) puts a code from an app after the password.
- **Changes ask again.** Every account and two-step change (a new account, a role, a password, a car list, turning
  someone's two-step sign-in off, new recovery codes, the two-step rule) asks for the acting account's password and,
  when it has two-step sign-in on, a code from the app or a recovery code: a browser left open plus a known password
  isn't enough. When the rule changes, open live streams of accounts it cuts off end at once.
- **What counts as the home network** is decided from the connection's address: loopback, the private ranges and
  link-local. Behind Docker's published ports that address is only right for IPv4: a connection to the host's
  **IPv6** address on a network without IPv6 (the compose file's default) is passed on by Docker's proxy from the
  bridge's gateway (for example `172.18.0.1`), so every IPv6 visitor from the internet looks like the home network,
  the "two-step from outside" rule doesn't hold for them, and their failed tries share one address. If the host has a
  public IPv6 address, don't open the ports to it in the firewall, or turn IPv6 on for the compose network
  (`enable_ipv6: true` with an IPv6 subnet) so the real address arrives. The same goes for a reverse proxy on the
  same host: it is the address the server sees.
- Sealing the key file to the host's TPM is optional, later.

## The Tesla API input

The server can read your cars from Tesla's Owner API (the unofficial API Tesla's own app uses), beside or instead of
SMO. How to add an account is in [install.md](install.md#add-a-tesla-account).

- **Off until an admin adds a Tesla account.** Before that the server starts nothing for it, makes no connection
  and no DNS lookup for any Tesla host. Removing the last account stops it again.
- **Only Tesla's own hosts, over TLS.** The hosts are fixed in the program, never taken from a setting or an answer:
  `auth.tesla.com`, `owner-api.teslamotors.com` and `streaming.vn.teslamotors.com`, or for a China account
  `auth.tesla.cn` and `owner-api.vn.cloud.tesla.cn`. TLS 1.3 with rustls and the public web roots; redirects are
  not followed. Through the owner's HTTP proxy only when one is set in Settings (`outbound.proxy`).
- **What is sent, and nothing else:** the token requests (a refresh of the access token, or the code from your
  sign-in), the list of the account's cars, one car's data, and, while a car drives, a subscription to Tesla's live
  stream for it. The requests carry the access token and a plain `SentryMate/<version> (self-hosted)` User-Agent.
  Nothing about this server, its other cars, its accounts or its owner is sent.
- **Read-only.** There is no code that wakes a car or sends it a command. A car that is asleep is left asleep: only
  the car list is read then, which does not wake it. An awake car with nothing going on is left alone for a while
  so it can fall asleep. At most one request a second and 3,000 a day go to Tesla per account.
- **The sign-in is on Tesla's own page,** in your browser: Sentry Mate never sees your Tesla password, your
  two-step code or a captcha. You paste back the address the page ends on; the code in it works once and only for
  that sign-in (it is bound to a secret that never leaves the server).
- **Tokens are sealed in the vault** (`tesla/accounts/<id>.json` in the data folder, encrypted with the vault key
  like everything else), held in memory only while used, and wiped after. No page, API answer, audit entry or log
  line shows a token. A move or an export carries the account to the new server, still sealed.
- **No full VIN is kept:** the VIN prefix, as for SMO, and a keyed hash to tell two cars with the same prefix apart.
  The access token names the account's e-mail inside; the server never reads it, and the e-mail is kept nowhere.
- **Admins only.** Every page and call about Tesla accounts is for admins, and every change asks for the admin's
  password again (and the two-step code when it is on) and writes an audit entry. When Tesla stops taking an
  account's token, the server stops asking (no retries against Tesla's sign-in), writes an audit entry and shows a
  notice to admins until someone signs in again.
- **Removing an account** stops reading at once and deletes its sealed file. Sign the server out at Tesla too
  (your Tesla account's security page), so the token stops working there as well.
- Data from the Tesla API is not signed like SMO's logs. Every row says where it came from (`tesla_api`), so it is
  never mistaken for checked CAN data.

## No way back without a backup

This version writes growing files in a newer format (format 2) as soon as it opens them to add to, from its first
start. Older versions can't open that format, so once it has run on a data folder, going back to an older image
needs a backup of the data folder made before the upgrade ([Backups](install.md#backups)).

## Privacy

- Logs hold the car's GPS position. Treat the data folder as private, and anything you export from it
  ([Logs](commands.md#logs)).
- Contents are sealed, names are not. A copied disk shows each car key, and each log's UTC start time (it is in the
  log's name), so it shows when each car was recording.
- The certificate (`tls/cert.pem`) is in the clear. It is public anyway.
- At the default log level (`info`) the server writes no log line per upload. With `RUST_LOG=debug`, the TLS
  library logs a few lines per connection (no names or addresses).

## When a log fails its check

Every log is kept as uploaded, whatever its check says. Each gets one of these results:

| Result | When |
|---|---|
| verified | Signed by the key the car key is paired with. The first signed log pairs it. A key trusted before keeps the logs that came while it was trusted. |
| unsigned | Not signed, and the car key isn't paired yet. |
| could not sign | Not signed, from a paired car key, and the bridge said why it couldn't sign. |
| failed | A bad signature or sidecar, a changed file, not signed although the car key is paired, or signed by another key. |
| waiting | The sidecar hasn't come. After 10 minutes the log is judged without it. |

- No command lists each log's result yet. `chain` shows the paired key's failed logs and the gaps
  ([The chain report](commands.md#the-chain-report)). `log decode` says one log's result.
- `log decode` decodes only `verified` and `unsigned` logs, unless `--any`. That is the only way to use a failed log
  today, one log at a time.
- A log signed by another key is marked failed. `keys` lists that key. Check it in the app, then trust it with
  `keys trust` ([A new signing key](#a-new-signing-key)): its logs are verified from then on.
- A reinstalled bridge app or a new Screenmate has a new car key. Its uploads with the old token get
  `403 wrong_car`, so give it a new token. Its logs are kept under the new car key, which is paired by its own first
  signed log. Linking several car keys to one car is planned.

## A new signing key

The CAN Bridge signs every log with a key kept in the Screenmate. The first signed log of a car key pairs it with
that key. A log signed by another key is kept, marked failed, and not used. That happens when the Screenmate's
keystore makes a new key, or when someone else, who has the car's upload token, uploads with it.

See the keys of a car. The car key is the last column of `token list`:

```bash
docker exec sentry-mate /sentry-mate keys <car key>
```

Each key shows as the CAN Bridge app shows it under **Log signing key**, with how many logs it signed, when its
first and last log came, and whether it is trusted. A new key says `not trusted: its logs are kept, not used`.

Then open the CAN Bridge app and look at **Log signing key**:

- **It is the new key.** Trust it. Copy the key from the list:

  ```bash
  docker exec -it sentry-mate /sentry-mate keys trust <car key> <signing key>
  ```

  It shows what will change and asks you to type `yes` and press Enter (`-it` lets you answer; `--yes` skips the
  question). Then the logs that key signed are used, and so are its later logs. The key trusted before stays trusted
  for the logs that came before; its later logs fail. It works with Sentry Mate running. Only a key that signed at
  least one of the car key's logs can be trusted.

- **It is not the new key.** Don't trust it: someone else signed those logs. Revoke the car's token, make a new one
  and put it in the app ([Tokens](commands.md#tokens), [The CAN Bridge app](cars-and-bridge.md#the-can-bridge-app)).
