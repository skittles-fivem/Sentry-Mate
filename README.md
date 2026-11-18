# Sentry Mate

A self-hosted car logger for Teslas fed by the **Screenmate CAN Bridge**: the car's own CAN bus data, uploaded by
the bridge app, instead of Tesla's API. One Rust program in one small Docker image, no database or dashboard
containers, no Tesla account, nothing that wakes the car.

**Status: early.** What runs today is receiving, checking and decoding the bridge's logs. The parts that work out
drives and charges, store them, and handle places and accounts are built and tested but not yet wired into the
program, and neither are the TeslaMate import and MQTT; the web UI is being built. The plan is in [docs/plans/rust-can-logger.md](docs/plans/rust-can-logger.md).

| Part | State |
|---|---|
| Upload API the bridge speaks (ping, status, resumable chunks), TLS 1.3 only | done |
| Logs kept exactly as uploaded, never replaced | done |
| One upload token per car, tied to its CAN key on first upload, no limit on cars | done |
| Any number of cars: kept per car, bounded memory, cars upload in parallel, an upload never reads a car's folder | done |
| Signature check, pairing with the car's key, chain report (gaps, swaps, restarts) | done |
| Reading both log formats (candump text, columns) | done |
| Decoding signals (DBC sets per model, bus split, the Screenmate's GPS), same values as Sentry Six Manager | done |
| Drives, charges, states, positions, charge curves from the decoded signals; the car's model from its VIN | built, not wired in yet |
| Storage for everything worked out (sealed, compressed columns, any number of cars, backups) | built, not wired in yet |
| Places, geofences, charge costs with time-of-use rates, addresses offline or online | built, not wired in yet |
| Accounts: admin and watch-only, per-car access, two-step sign-in, sessions, audit log | built, not wired in yet |
| Encryption at rest (pure Rust): everything stored is sealed, key file + recovery key | done |
| Moving to another server: a direct move with a one-time move code (two lines in the new server's compose file, no shell), or one export file (opens with the recovery key) | done, see [`docs/plans/move-server.md`](docs/plans/move-server.md) |
| TeslaMate import from TeslaMate's own backup file (any number of cars, streamed, again later adds only what's new) | built, not wired in yet |
| MQTT for Home Assistant (TLS only, discovery, any number of cars on one connection) | built, not wired in yet |
| Web UI: UI packs, two designs as prototypes on made-up data in [`ui/`](ui/), maps; every TeslaMate tab gets a clearer page, planned in [`docs/parity/ui-parity.md`](docs/parity/ui-parity.md) with the API in [`docs/ui-api.md`](docs/ui-api.md) | being built |

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

```bash
docker compose exec sentry-mate /sentry-mate log decode <log name> > signals.csv
```

New machine, or the key file lost: bring the data folder, then type the recovery key:

```bash
docker compose run --rm -it sentry-mate restore
```

Replace the recovery key (the old one stops working):

```bash
docker compose exec sentry-mate /sentry-mate recovery-key new
```

### Move to another server

The new server pulls everything from the old one over the upload port, while the old one keeps taking uploads, then
takes over by itself. The logs, uploads still arriving, the tokens, the trust records and the TLS key all move, so
the CAN Bridge keeps its token and its certificate SHA-256.

1. **On the old server, get a move code** (valid 24 hours, works once):

   ```bash
   docker compose exec sentry-mate /sentry-mate move code
   ```

   With no shell (TrueNAS): add `SENTRY_MATE_OFFER_MOVE=yes` to its environment, redeploy, and copy the code from
   its log.

2. **Install the new server with empty `data` and `keys` folders and two more lines**, then start it:

   ```yaml
       environment:
         - SENTRY_MATE_MOVE_FROM=192.168.1.20
         - SENTRY_MATE_MOVE_CODE=SMMV-1K3QX-7WD2M-ZP4HT-9BN6R-XC8VA-5JE0F
   ```

   (Made-up values: the old server's address as the CAN Bridge uses it, and the code from step 1.) Its log shows the
   copy and ends with `move: finished`. The old server's log then says `move: done`: only then stop it.

3. **If the new server has another address, change it in the CAN Bridge app.** Nothing else changes. The bridge keeps
   its logs meanwhile and sends them once it reaches the new server.

The two lines can stay; they are ignored once the move is done. The old server answers the bridge "moved, try later"
and takes no more uploads; `move undo` there makes it take them again. `move status` shows what a move is doing on
either server, `move cancel` stops it. On one machine (a new disk or pool), give the new app another host port for
the move (`10101:10100`), then port 10100 back once the old app is stopped.

The code proves itself over the TLS connection, so a fake server or someone in between learns nothing; the data key
travels only at the end, wrapped with a key from the code, and the key file never leaves its folder.

**Export file**, for when the two servers can't reach each other, or to keep a copy. One export file holds everything: the logs, uploads still arriving, the tokens, the trust records and the TLS key,
all sealed as stored. It opens only with the recovery key, never with the file alone. Make it while the server runs
(uploads wait a few seconds):

```bash
docker compose exec -T sentry-mate /sentry-mate export > sentry-mate.smexport
```

On the new server (empty `data` and `keys` folders, started once or not at all), put the file in its data folder,
readable by the server's user, then:

```bash
docker compose exec sentry-mate /sentry-mate import /data/sentry-mate.smexport
```

It asks for the old server's recovery key, checks the whole file, and only then switches to it; a running server
switches by itself. A stop part way is fine: run it again and it carries on. The CAN Bridge keeps its token and its
certificate SHA-256: change only the server address, if it changed. The old recovery key opens the data there too.

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
  open. Files that grow (uploads still arriving, journals) give every piece its own random nonce, so a piece written
  again after a crash never reuses one, and bind each piece to the one before it, so pieces can't be reordered,
  dropped from the middle or mixed with those of an older copy of the file (an older copy put back whole, or pieces
  missing at the end, can't be told from a crash). The data key is kept only wrapped: by the key file in `/keys` (a
  separate volume, refused inside the data folder) for restarts with nothing to type, and by the recovery key
  through Argon2id at 1 GiB. Keys are wiped from memory when no longer needed and core dumps are off. This protects
  a stolen or copied disk or backup; it can't protect a server taken over while it runs.
- **No way back without a backup.** This version writes growing files in a newer format (format 2) as soon as it
  opens them to add to, from its first start. Older versions can't open that format, so once it has run on a data
  folder, going back to an older image needs a backup of the data folder made before the upgrade.
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

Crates: `logfmt` (reads the logs), `trust` (signatures and chain), `vault` (encryption at rest), `model` (the
shared types), `dbc` (decodes signals), `ingest` (upload API, storage, tokens, TLS server), `timeline` (states,
drives, charges from the signals), `db` (sealed storage for what the timeline works out), `places` (geofences,
prices, addresses, map settings), `accounts` (people and sign-in), `import` (TeslaMate history), `mqtt` (Home Assistant), `sentry-mate` (the program).

Sentry Mate is written from scratch and copies no code from TeslaMate; it is not affiliated with TeslaMate, Tesla,
or Screenmate.
