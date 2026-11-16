# Sentry Mate

A self-hosted car logger for Teslas fed by the **Screenmate CAN Bridge**: the car's own CAN bus data, uploaded by
the bridge app, instead of Tesla's API. One Rust program in one small Docker image, no database or dashboard
containers, no Tesla account, nothing that wakes the car.

**Status: early.** What works today is receiving and checking the bridge's logs. Drives, charges, battery health,
the web UI and the rest are being built; the plan is in [docs/plans/rust-can-logger.md](docs/plans/rust-can-logger.md).

| Part | State |
|---|---|
| Upload API the bridge speaks (ping, status, resumable chunks), TLS 1.3 only | done |
| Logs kept exactly as uploaded, never replaced | done |
| One upload token per car, tied to its CAN key on first upload | done |
| Signature check, pairing with the car's key, chain report (gaps, swaps, restarts) | done |
| Reading both log formats (candump text, columns) | done |
| Decoding signals, drives, charges, states | next |
| Encryption at rest (pure Rust): everything stored is sealed, key file + recovery key | done |
| Web UI, accounts, maps, TeslaMate import | planned |

## Run it

```yaml
# docker-compose.yml
services:
  sentry-mate:
    build: .
    restart: unless-stopped
    ports:
      - "10100:10100"   # CAN Bridge uploads, HTTPS (TLS 1.3 only)
    volumes:
      - ./data:/data    # everything, sealed
      - ./keys:/keys    # the key file that opens it (its own folder, ideally another disk)
    read_only: true
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
    user: "1000:1000"
```

```bash
mkdir -p data keys && sudo chown 1000:1000 data keys
```

```bash
docker compose up -d --build
```

On first start Sentry Mate makes its vault and puts the **recovery key** in `keys/RECOVERY-KEY.txt` (not in the
log). Copy it somewhere safe away from the server (a password manager, paper), then delete that file. With the key
file it is the only way to open the data; lose both and nobody can.

Make an upload token for the car (shown once):

```bash
docker compose exec sentry-mate /sentry-mate token new --car "Model Y"
```

In the CAN Bridge app, fill in **Second Sentry Six Manager** with what that prints: the address
`https://<this server>:10100/api/can/logs`, the token, and the certificate SHA-256. The bridge then sends every log
to Sentry Mate as well as to Sentry Six Manager, each on its own.

Other commands:

```bash
docker compose exec sentry-mate /sentry-mate token list
```

```bash
docker compose exec sentry-mate /sentry-mate fingerprint
```

```bash
docker compose exec sentry-mate /sentry-mate chain <car key>
```

New machine, or the key file lost: bring the data folder, then type the recovery key:

```bash
docker compose run --rm -it sentry-mate restore
```

Replace the recovery key (the old one stops working):

```bash
docker compose exec sentry-mate /sentry-mate recovery-key new
```

## Security

- TLS 1.3 only on the upload port; plain HTTP and older TLS are refused. The certificate is made on first start
  (its key readable only by the server's user); the app pins it by SHA-256.
- Tokens are write-only, one per car, stored only as their SHA-256, tied to one CAN key on the first upload.
  Wrong tokens are rate limited per address; a right token is never refused because of wrong ones.
- Every log's ECDSA P-256 signature is checked against the key the car is paired with; a log signed by another
  key is kept but not used until the owner trusts that key. Deleted, swapped or reordered logs show in the chain.
- **Encryption at rest, in Rust.** One random data key seals everything stored (logs, sidecars, uploads still
  arriving, trust records, tokens, the TLS key) with XChaCha20-Poly1305, in 64 KiB chunks, each file with its own
  key (HKDF-SHA256) and its path bound in, so a file that is changed, cut short, or moved under another name doesn't
  open. The data key is kept only wrapped: by the key file in `/keys` (a separate volume, refused inside the data
  folder) for restarts with nothing to type, and by the recovery key through Argon2id at 1 GiB. Keys are wiped from
  memory when no longer needed and core dumps are off. This protects a stolen or copied disk or backup; it can't
  protect a server taken over while it runs.
- Pure Rust where it matters (`unsafe` forbidden in this code, the xz reader included); files from the network are
  read with fixed limits. The image is one static binary on `scratch`, run as a normal user.
- Nothing leaves the server. Logs hold the car's GPS position: treat the data folder as private.

## Development

```bash
cargo test --workspace
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Crates: `logfmt` (reads the logs), `trust` (signatures and chain), `ingest` (upload API, storage, tokens, TLS
server), `sentry-mate` (the program).

Sentry Mate is written from scratch and copies no code from TeslaMate; it is not affiliated with TeslaMate, Tesla,
or Screenmate.
