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

That is the **fast set**: every unit test and the quick integration tests (cargo runs one test binary at a time). It
still takes each main feature end to end at least
once: logs uploaded, checked and decoded into a drive and a charge on the web API (one made-up light day), sign-in and
accounts, places, the TeslaMate, TeslaFi and CAN imports, the Tesla API client against its fake server, moving and the export
file, the certificate, the server settings and MQTT, and the packs served. CI runs it in four parts on four machines
at once, each part a plain `cargo test`; run one part locally the same way:

```bash
cargo test --locked -p web
cargo test --locked -p synth -p engine -p moving
cargo test --locked -p import -p places -p sentry-mate -p accounts -p timeline
cargo test --locked --workspace --exclude web --exclude synth --exclude engine --exclude moving \
  --exclude import --exclude places --exclude sentry-mate --exclude accounts --exclude timeline
```

The **full set** adds the slow tests, marked `#[ignore = "slow: full tests (cargo test -- --include-ignored)"]`:
the tests that each decode several days of made-up logs through the whole engine at opt-level 0 (reports, places,
charges, battery, settings, drives, the system pages; 3 to 6 minutes each on CI's 4 cores), the long move, the
2,000-car upload and database, and the big CAN export imported in bounded memory. They take most of half an hour more:

```bash
cargo test --workspace --locked -- --include-ignored --skip measure_
```

(`--skip measure_` leaves out the two measurements in `crates/db/tests/compression.rs`, which are for a `--release`
run by hand.) Only the slow ones: `cargo test --workspace --locked -- --ignored`. One file's, for example:
`cargo test --workspace --locked --test places -- --include-ignored`. A test that takes more than about 20 seconds
on 4 cores belongs in the full set; a feature whose only tests are slow gets a small fast one too (no logs, or one
light day).

## CI and the published image

CI is [`.github/workflows/image.yml`](../.github/workflows/image.yml). It runs on a push to `main` that changes code
(a push that changes only `README.md`, `docs/` or `truenas/` runs nothing), and by hand (*Actions → Image → Run
workflow*). The owner's limit is 5 minutes for the whole run.
First the gates (above), once: the fast set in its four parts, and `cargo fmt` with clippy (both commands) beside
them on a machine of its own.

What is known about that limit, from the Image workflow's runs (ubuntu-latest, 4 cores):

- Run 24 took 7.5 minutes. The tests: about a minute to build each part's workspace crates (the dependencies come
  from GitHub's cache, `Swatinem/rust-cache`, one per part, written only by runs on `main`), then the test binaries one
  after another: 300 s for `web` (of which `web/tests/firmware.rs`, 190 s, is now in the full set), 34 s for
  `moving`, 16 s for `sentry-mate`, 164 s for every other crate together. So the fast set is now in four parts of
  45 to 105 s of tests each, parted by those times.
- The image: in the Docker build every workspace crate was built again each run (the dependencies were a cached
  layer): 4.5 minutes on arm64, 6 on amd64 (with full LTO and one codegen unit 9, so the release profile in
  `Cargo.toml` has neither). Now `.github/build-binary.sh` builds the binary on each arch's runner, in the
  Dockerfile's own pinned Rust image with the Dockerfile's command, with its build folder (`target/` and the downloaded
  crates) kept per arch as an artifact of `main`'s last run (14 days), so a push builds again only the crates it changed and
  those that use them, and those incremental (measured on 4 cores: a change to `web` or `engine` 20 to 40 s, where
  `web` and `sentry-mate` alone took 3.5 minutes without). The image is then made from the Dockerfile with that binary in place of its two build stages
  (`build-contexts: binary=`); a plain `docker build .` still builds them. The first run after a change of the Rust
  image or with no kept folder builds everything once.
- Everything else takes about 80 seconds (the jobs starting, the image check about 20 s, the push by digest under
  10 s, the tags about 15 s).

The times of each job are on the run's page (*Actions → Image*).

The full set runs in [`.github/workflows/full-tests.yml`](../.github/workflows/full-tests.yml): every Monday and by
hand (*Actions → Full tests → Run workflow*). It publishes nothing; a failure there shows on the Actions page.

At the same time it builds the image for amd64 and arm64, each on a machine of its kind. It checks each one with
[`.github/check-image.sh`](../.github/check-image.sh), as 1000:1000 and as TrueNAS's 568:568: folders it can't keep
are refused (root's, not mounted, the keys inside the data, the same folder at both), no capabilities, a read-only
root, healthy, TLS 1.3 only, the recovery key file, a token made with `docker exec`, a clean stop, and the same
certificate after a restart. Only when all of that passed does it publish both as one image,
`ghcr.io/skittles-fivem/sentry-mate`, tagged `latest`, `build-N` (the workflow's run number) and `sha-<commit>`.
`latest` is tagged last. The binary in it is built by `.github/build-binary.sh` with the build folder of `main`'s last
run (above); a plain `docker build .` builds the dependencies in a layer of their own (the `manifests` stage of the
Dockerfile builds them from the Cargo files alone), then the workspace's own crates.
A run by hand for another branch gets the branch's name as its tag, never `latest` or a
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
Swap `instrument` for `logbook`, `horizon`, `console`, `pebble` or `manager` to preview another of the six built-in packs.

Before a pack change goes in, check its pages ([`ui/tools/README.md`](../ui/tools/README.md)): the normal check at
`1440x900,390x844`, `--pixel` at `1440x900,3840x2160,390x844@3`, and `--stable` at `1440x900,390x844`.

The stability rule (the owner's): a page you are on must not jump around, and nothing may show up, go away and
come back while live updates arrive. A refresh keeps what is on screen until the new data is there (no loading
text, spinner or empty state in between), a block that fills in later has its room from the start, and a value
changes its text in place. `--stable` stays on each page for 30 s while the dev server sends live values and list
changes, and fails a page whose layout shifts (more than 0.01), whose blocks move, or whose elements come and go.

```bash
node ui/tools/check.js --pack instrument --stable --sizes 1440x900,390x844
```

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
