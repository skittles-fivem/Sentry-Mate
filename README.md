# Sentry Mate

A self-hosted Tesla logger, like TeslaMate, fed by the **Screenmate CAN Bridge** (SMO): the car's own CAN data, so
nothing ever wakes the car. One Rust program in one small Docker image: no database or dashboard containers.

Optional extra inputs: a **Tesla account** (the Owner API, off until you add one) and **imports** from TeslaMate, TeslaFi or
Sentry Six Manager. Every drive, charge and value says where it came from.

**Status: early.** [Progress](docs/status.md) · [To do](docs/todo.md)

## Install

- **TrueNAS SCALE:** a custom app from [`truenas/sentry-mate.yaml`](truenas/sentry-mate.yaml):
  [steps](docs/install.md#truenas-scale).
- **Docker Compose:** [`docker-compose.yml`](docker-compose.yml): [steps](docs/install.md#docker-compose).

Then open `https://<server>:10101/`, type the set-up code from the log, and make the first admin. Point the CAN
Bridge app at `https://<server>:10100/api/can/logs` with a token from **Settings → Cars**
([The CAN Bridge app](docs/cars-and-bridge.md#the-can-bridge-app)).

## Guides

| | |
|---|---|
| [Install](docs/install.md) | Compose, TrueNAS, updates, backups, restore, Tesla account, ports, when it won't start |
| [Cars and the bridge](docs/cars-and-bridge.md) | Supported cars, the CAN Bridge app, imports from Sentry Six Manager |
| [Moving](docs/moving.md) | Another server in one step: a move code or an export file |
| [Commands](docs/commands.md) | Every command, setting, port, limit and file |
| [Security](docs/security.md) | Encryption at rest, accounts, what leaves the server |
| [Rebuilds](docs/rebuilds.md) | What makes the server work its history out again, and why it never does so twice |
| [UI packs](ui/README.md) | The six built-in designs |
| [Development](docs/development.md) | Building, tests, CI, the crates |

## Licence

None chosen yet. Bundled third-party files keep their own licences
([DBC files](crates/dbc/files/README.md), [UI packs](ui/README.md); the time zone boundaries built into the
server, from the `tzf-dist` crate, are timezone-boundary-builder's data from © OpenStreetMap contributors, ODbL). Written from scratch; not affiliated with
TeslaMate, Tesla or Screenmate.
