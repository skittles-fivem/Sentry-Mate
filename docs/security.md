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
  SHA-256 and tied to one car key on the first upload. Wrong tokens are rate limited per address; a right
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
- Nothing leaves the server: while it runs, Sentry Mate makes no outgoing connections.
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
