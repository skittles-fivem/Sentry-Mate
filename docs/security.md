# Security and privacy

[← Back to the README](../README.md)

- [Security](#security)
- [No way back without a backup](#no-way-back-without-a-backup)
- [Privacy](#privacy)
- [When a log fails its check](#when-a-log-fails-its-check)
- [A new signing key](#a-new-signing-key)

## Security

- TLS 1.3 only on the upload port; plain HTTP and older TLS are refused. The certificate is self-signed and made on
  first start, its private key sealed in the vault; the app pins it by SHA-256.
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
- **The container.** The image runs as 1000:1000; the TrueNAS file runs it as TrueNAS's apps user, 568:568. The
  compose and TrueNAS files drop every capability, make the root read-only and set `no-new-privileges`, so it can
  write only to its two mounted folders.
- **Folders it can't keep are refused**, before anything is written: a data or keys folder that is not mounted (it
  would be lost with the container, the key file with it), a keys folder inside the data folder or the same folder
  at both (the key file would be in the data's backups), and a folder it can't write to (most often root's).
- **A new vault is never made over an existing key file.** An empty or wrong data folder next to a key file stops it
  instead, so the key file that opens your data is never replaced.
- Nothing leaves the server: while it runs, Sentry Mate makes no outgoing connections. The one exception is a new
  server during a move you started: it connects to the old server's upload port, and nowhere else.
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
