# Cars and the CAN Bridge

[← Back to the README](../README.md)

- [Supported cars](#supported-cars)
- [The CAN Bridge app](#the-can-bridge-app)
- [Check it works](#check-it-works)
- [How the bridge uploads](#how-the-bridge-uploads)
- [Running next to TeslaMate and Sentry Six Manager](#running-next-to-teslamate-and-sentry-six-manager)
- [Bringing in the logs Sentry Six Manager already has](#bringing-in-the-logs-sentry-six-manager-already-has)

## Supported cars

| Car | DBC set | Checked on a real car |
|---|---|---|
| Model 3 and Model Y, every year | `tesla-3y` | Model Y only |
| Model S and Model X from 2021 | `tesla-3y` | no |
| Model S and Model X before 2021 | `tesla-sx-legacy` | no |

Today only `log decode` decodes ([Logs](commands.md#logs)). It picks the set from the car's VIN (the log's own
frames, its sidecar, or the car key's newest logs), else `tesla-3y`. `--model` picks one by hand. The decoded values
are the same as Sentry Six Manager's, the Screenmate's GPS included. The DBC files and their sources are listed in
[crates/dbc/files/README.md](../crates/dbc/files/README.md).

## The CAN Bridge app

You need the Screenmate CAN Bridge app, build 149 or later (it has the **Second Sentry Six Manager (optional)**
section), able to reach this server. The build number is at the top of the app. Update it in the app's **Updates**
section.

First make an upload token on the server ([Tokens](commands.md#tokens)). It prints the three fields for the app.

`<server>` in the address below is the server's name or LAN address, not the `0.0.0.0` in the log. This shows
its addresses:

```bash
hostname -I
```

Give the server a fixed address in your router, so uploads don't stop when it changes.

On the Screenmate, open the CAN Bridge app. Under **Second Sentry Six Manager (optional)**, fill in what
`token new` printed:

| App field | What to put in |
|---|---|
| Upload address (empty = off) | `https://<server>:10100/api/can/logs`, for example `https://192.168.1.20:10100/api/can/logs`. |
| Token | The line that starts with `smt_`. |
| Certificate SHA-256 | The 64 hex digits that `token new` printed. |

Then press **Apply & restart** at the top of the app. The app saves the fields only then.

- `token new` prints the port inside the container (10100 unless `SENTRY_MATE_UPLOAD_PORT` is set). If you map
  another host port (for example `"10200:10100"`), use the host port.
- The certificate names only `localhost` unless `SENTRY_MATE_NAMES` was set when it was made, but the app pins the
  SHA-256 and does not check names.
- No Sentry Six Manager? Sentry Mate speaks the same upload API, so the app's first section, **Sentry Six Manager
  (CAN log upload)**, should work the same way. This is not tested yet.
- The bridge's messages say "Sentry Manager" in both sections.

## Check it works

First check that the server can be reached from the car's Wi-Fi: on a phone or laptop on that network, open
`https://<server>:10100/api/can/logs/ping` and accept the certificate warning. An answer with `bad_token` means the
address, the port and the firewall are right (the browser has no token). The upload address itself, without
`/ping`, answers `Nothing here.`, and that is normal too.

The bridge finishes a log about every minute while the car is awake, and none while it sleeps. Wake the car (open a
door) and wait a minute or two. The app's **Second Sentry Manager upload** card shows how it is going;
**Upload now** tries at once.

Check that the bridge reached Sentry Mate. The token's last column changes from `not tied to a car key yet` to the
car key:

```bash
docker exec sentry-mate /sentry-mate token list
```

The token is tied when the first piece of a log comes in, before it is stored. So this shows that the bridge got
through with this token, not that a whole log arrived. The logs that arrived are in the car key's folder:

```bash
sudo ls data/can/<car key>
```

What the bridge's messages mean, and what to do: [When it does not start](install.md#when-it-does-not-start), the
second table.

## How the bridge uploads

- The bridge sends every finished log to Sentry Mate as well as to Sentry Six Manager, each on its own.
- **Upload only on Wi-Fi**, in the app's **Sentry Six Manager (CAN log upload)** section, holds uploads to Sentry
  Mate too.
- Sentry Mate accepts both log formats, so the bridge keeps writing the smaller columns format. It writes that
  format only while both servers accept it.
- While a server can't be reached, logs wait on the Screenmate up to the app's **Keep up to (GB)** (default 15).
  Then the oldest are removed, first those both servers have. So if Sentry Mate is down for long, it can miss logs.
- The CAN Bridge's Raspberry Pi logger uploads the same way. It has one upload address, so it sends either to
  Sentry Six Manager or to Sentry Mate. It needs a token of its own: its car key is not the Screenmate's.
- A reinstalled bridge app or a new Screenmate has a new car key, and needs a new token
  ([When a log fails its check](security.md#when-a-log-fails-its-check)).

## Running next to TeslaMate and Sentry Six Manager

- **Sentry Six Manager** uses ports 9000 (web) and 10000 (CAN uploads). Sentry Mate uses 10100, and 10101 for its web
  UI, so both run on one server. Sentry Six Manager needs no change: the bridge sends to each on its
  own.
- **TeslaMate**: Sentry Mate doesn't talk to TeslaMate, its database, Tesla or the car. Keep TeslaMate running as
  long as you like. Its history comes in from TeslaMate's own backup file: Settings → Import in the web UI.

## Bringing in the logs Sentry Six Manager already has

The bridge sends Sentry Mate only the logs it finishes from the day its second server is set up. The logs Sentry
Six Manager received before that come over in one file, and become drives, charges and history here as if the
bridge had uploaded them:

1. **In Sentry Six Manager** (an admin account), open the **Nerd panel** and choose **Logs + ids.csv (.zip, any
   range)**: the CAN export bundle (`GET /api/can/export/bundle`). Pick the car's CAN Bridge **car key**, and for all
   time set **From** to a day before its first log and **To** to now. Leave "as candump text" off: the import uses the signed `.log.xz`
   files and only counts the text copies. One zip per car key; it can be any size (past 4 GB it is a Zip64 file,
   which is read the same).
2. **In Sentry Mate**, open **Settings → Import** as an admin and choose that `.zip` as it was downloaded (don't
   unpack or zip it again). It uploads in chunks over HTTPS and can stop and go on.
3. **The check** reads the whole zip and stores nothing. Its report says how many logs it would import, how many are
   here already (the bridge uploaded them to both servers) and how many it would refuse and why, per car key, and
   which car each key goes to.
4. **Start the import.** Each log is checked as an upload is (the signature, the signing key paired with the car key,
   the hash chain), stored and decoded into the car's history. A key a car has goes to that car; a new key goes
   where a new key's first upload would (Settings → Cars). The rows are the car's own CAN data, not "imported"
   rows.

What it never does: store a log that is not signed by the car's CAN Bridge, or changed since it was signed (those
are refused and counted, with why), or store a log twice (a log already here, by name and SHA-256, is skipped).
Stopping it part way loses nothing: start it again, or import the same zip again later, and what is here is skipped.
A gap or fork in a key's chain shows on Settings → Cars as it does for uploads. The details:
[`docs/ui-api.md`](ui-api.md#sentry-six-managers-can-export).
