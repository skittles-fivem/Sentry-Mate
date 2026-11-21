# Development

[← Back to the README](../README.md)

- [Toolchain](#toolchain)
- [The gates](#the-gates)
- [CI and the published image](#ci-and-the-published-image)
- [Checking an image you built](#checking-an-image-you-built)
- [Running your own build](#running-your-own-build)
- [After the first publish](#after-the-first-publish)
- [Pure Rust](#pure-rust)
- [Previewing the UI packs](#previewing-the-ui-packs)
- [Crates](#crates)
- [Layout](#layout)

## Toolchain

Rust 1.88 or newer (`rust-version`: the locked `time` and `sha2` crates need 1.88), edition 2024. `unsafe` is
forbidden in every crate here, and clippy denies `unwrap`.

Building the image needs a clone, internet for the Rust build image and crates, and about 3 GB of free disk.

## The gates

```bash
cargo fmt --all --check
```

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy -p sentry-mate --all-targets --features dev-seed --locked -- -D warnings
```

The second checks the hidden `dev seed` command, which only a build with the `dev-seed` feature has (made-up cars
for development: `cargo run -p sentry-mate --features dev-seed -- --data DIR --keys DIR dev seed --days 3 --light`,
into a data folder of its own). Then `serve` with the same `--data` and `--keys` decodes the made-up logs and shows
them on the web port (`--web-port`, 10101); the setup code for the first admin is in the log and in the keys folder's
`SETUP-CODE.txt`. The end-to-end test (`crates/sentry-mate/src/e2e.rs`) does the same in-process: seed, `serve` on
free ports, setup, the cars, drives and charges, and the built-in pack with its CSP.

```bash
cargo test --workspace --locked
```

## CI and the published image

CI is [`.github/workflows/image.yml`](../.github/workflows/image.yml). It runs on a push to `main` that changes code
(a push that changes only `README.md`, `docs/` or `truenas/` runs nothing), and by hand (*Actions → Image → Run
workflow*). First the gates (above), once.

At the same time it builds the image for amd64 and arm64, each on a machine of its kind. It checks each one with
[`.github/check-image.sh`](../.github/check-image.sh), as 1000:1000 and as TrueNAS's 568:568: folders it can't keep
are refused (root's, not mounted, the keys inside the data, the same folder at both), no capabilities, a read-only
root, healthy, TLS 1.3 only, the recovery key file, a token made with `docker exec`, a clean stop, and the same
certificate after a restart. Only when all of that passed does it publish both as one image,
`ghcr.io/skittles-fivem/sentry-mate`, tagged `latest`, `build-N` (the workflow's run number) and `sha-<commit>`.
`latest` is tagged last. A run by hand for another branch gets the branch's name as its tag, never `latest` or a
`build-N`.

`cargo deny`, `cargo audit` and fuzzing in CI are planned.

## Checking an image you built

Check an image you built yourself the same way. The check needs docker, curl and openssl, and sudo when not run as
root. It uses port 10100 on 127.0.0.1 (`CHECK_PORT` picks another, for a server that runs Sentry Mate already):

```bash
docker build -t sentry-mate:check .
```

```bash
.github/check-image.sh sentry-mate:check
```

```bash
.github/check-image.sh sentry-mate:check 568
```

## Running your own build

A local build shows no build number in `--version`. To run your own build instead of the published one, turn on the
OPTIONAL `build: .` line in `docker-compose.yml` (in a clone) and start it with `docker compose up -d --build`. Your
build takes the published image's name. A later `docker compose pull` puts the published image back.

To update it: `git pull`, then `docker compose up -d --build`. The build leaves out `data/`, `keys/` and `.env`
(`.dockerignore`).

## After the first publish

The repo owner, once. GitHub makes a new package private. Until it is public, every pull fails with `denied`, on
Docker Compose and on TrueNAS. Make it public once: on GitHub, *Packages → sentry-mate → Package settings → Change
visibility → Public*. Then check that it pulls without a login:

```bash
docker pull ghcr.io/skittles-fivem/sentry-mate:latest
```

## Pure Rust

The aim is pure Rust. Today only `ring` (used by rustls and rcgen for TLS) builds C code. Check that nothing else
does:

```bash
cargo tree -e normal,build -i cc --depth 1
```

## Previewing the UI packs

The packs ([`ui/README.md`](../ui/README.md)) are built into the server and served on its web port. To work on them
without a server, the dev API server answers all of `/api/v1` on made-up data
([`ui/devserver/README.md`](../ui/devserver/README.md)):

```bash
python3 ui/devserver/serve.py --pack ui/packs/instrument --port 8000
```

Then open `http://127.0.0.1:8000/`. The pages load map tiles from `tile.openstreetmap.org`.
Swap `instrument` for `logbook`, `horizon`, `console` or `pebble` to preview another of the five built-in packs.

## Crates

| Crate | Job | In the program |
|---|---|---|
| `logfmt` | Reads the logs (both formats, xz in pure Rust) | yes |
| `trust` | Signatures, pairing, chain report | yes |
| `vault` | Encryption at rest | yes |
| `dbc` | Decodes signals | yes |
| `ingest` | Upload API, storage, tokens, trust records, TLS server | yes |
| `gate` | One server per data folder, and the pause of the writes an export or a move takes | yes |
| `moving` | The export file, the import and the direct move (`docs/plans/move-server.md`) | yes |
| `model` | The shared types | yes |
| `timeline` | States, drives, charges from the signals | yes, through the engine |
| `db` | Sealed storage for what the timeline works out | yes |
| `places` | Geofences, prices, addresses, map settings | yes, through the engine and the web server (Places page) |
| `accounts` | People and sign-in | yes, through the web server |
| `import` | TeslaMate's backup and TeslaFi's data download | yes, through the engine (the web UI's import page) |
| `engine` | Checked logs into each car's history in the background, and the read side of `/api/v1` | yes |
| `web` | The web port: UI packs, sign-in, accounts, preferences, the live stream, the frame every `/api/v1` route shares | yes |
| `mqtt` | Home Assistant | yes, through the engine and the web server |
| `synth` | Made-up CAN Bridge logs (signed, chained, both formats) for tests | only the hidden `dev seed`, in a build with `--features dev-seed` (never the image) |
| `sentry-mate` | The program: `serve` runs the upload port, the engine and the web port together (`src/site.rs`); the `/api/v1` reads over the engine are in `src/api.rs` | |

## Layout

| Path | What |
|---|---|
| `crates/` | The Rust workspace |
| `crates/dbc/files/` | The DBC files, with their licences |
| `docs/plans/` | The plan |
| `docs/parity/`, `docs/ui-api.md` | The UI plan and the API it needs |
| `ui/packs/` | The UI packs |
| `Dockerfile`, `docker-compose.yml` | The image and how to run it |
| `truenas/sentry-mate.yaml` | The TrueNAS SCALE app |
| `docs/install.md` | The install guide |
| `docs/commands.md`, `docs/moving.md`, `docs/security.md`, `docs/cars-and-bridge.md`, `docs/development.md` | The other guides |
| `tools/move-e2e.sh` | The move's end-to-end test: two servers in Docker |
| `.github/workflows/image.yml`, `.github/check-image.sh` | CI: the gates, the image, its check, publishing |
