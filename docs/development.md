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
```

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

The packs ([`ui/README.md`](../ui/README.md)) run on made-up data from `web/mock-api.js`; no server serves them yet.
Preview a pack with any static file server, from the repo's root:

```bash
python3 -m http.server -d ui/packs/instrument/web 8000
```

```bash
python3 -m http.server -d ui/packs/logbook/web 8000
```

Then open `http://127.0.0.1:8000/`. Add `?cars=200000` to try a large fleet, `?latency=0` to drop the simulated
network delay. The previews load map tiles from `tile.openstreetmap.org`.

## Crates

| Crate | Job | In the program |
|---|---|---|
| `logfmt` | Reads the logs (both formats, xz in pure Rust) | yes |
| `trust` | Signatures, pairing, chain report | yes |
| `vault` | Encryption at rest | yes |
| `dbc` | Decodes signals | only `log decode` |
| `ingest` | Upload API, storage, tokens, trust records, TLS server | yes |
| `model` | The shared types | only the signal set from the VIN |
| `timeline` | States, drives, charges from the signals | only the VIN reader, in `log decode` |
| `db` | Sealed storage for what the timeline works out | not yet |
| `places` | Geofences, prices, addresses, map settings | not yet |
| `accounts` | People and sign-in | not yet |
| `import` | TeslaMate history | not yet |
| `mqtt` | Home Assistant | not yet |
| `sentry-mate` | The program | |

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
| `docs/commands.md`, `docs/security.md`, `docs/cars-and-bridge.md`, `docs/development.md` | The other guides |
| `.github/workflows/image.yml`, `.github/check-image.sh` | CI: the gates, the image, its check, publishing |
