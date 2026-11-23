# `wanted`: logs the server asks the CAN Bridge app to send again

For the CAN Bridge (SMO) app. Everything else of the upload API is as before (Sentry Six Manager's
`docs/can-bridge-api.md`); this adds one field to the ping's answer and one to the status call, and one refusal.
Sentry Mate's side: [cars-and-bridge.md](cars-and-bridge.md#did-each-log-arrive-intact).

## Why

A server can lose a stored log after the app checked it as `verified` and deleted it from the car (a disk
mishap, a restore from an older copy, a file removed by hand). Until now the server only answered about names the
app asked about, so a log the app no longer asked about was never sent again. The server now finds the logs it
should have (from its records of what arrived, and from the next log's `prev_name`) and says so.

## In the ping

`GET /api/can/logs/ping` answers, beside the fields it had:

```json
{"ok": true, "api": 1, "vehicleKey": "k", "maxParallel": 16, "wanted": [
  "k_20261112T081500Z_0007.log.xz",
  "k_20261112T081500Z_0007.log.xz.json",
  "k_20261113T170200Z_0003.log.xz"
]}
```

- `wanted`: file names (logs and sidecars) of the token's car the server is missing, oldest first (by the start
  in the name, then the sequence, a log before its sidecar), at most 500. Empty when nothing is missing, and for a
  token not tied to a car yet. An app that doesn't know the field ignores it, as before.
- The list is the server's view as of its last scan (at start-up and once a day) plus what arrived since: a name
  stays on it until it arrives again, or the app says it no longer has it.

## What the app does

1. After a ping with a non-empty `wanted`, for each name it **still holds** (in its outbox, its kept logs, or
   anywhere it keeps verified logs before deleting them): upload it again exactly as it was stored, the usual way
   (status call, then chunks at the offset the status gives; a name the server lost answers `missing`, so the
   upload starts at 0). Send the log and its sidecar both when both are listed. Resend the same bytes: never write
   a log again or sign it again.
2. For each name it **no longer holds**, say so in its next status call, in `not_held`:

   ```json
   {"names": ["k_20261113T170200Z_0003.log.xz"], "not_held": ["k_20261112T081500Z_0007.log.xz",
     "k_20261112T081500Z_0007.log.xz.json"]}
   ```

   `not_held` is optional, up to 2000 names; names that are not wanted are ignored. The server then stops asking
   for them and shows the owner they are lost for good. Naming one in `names` later (the app found it after all)
   puts it back on the list.
3. Wanted uploads go with the others (`maxParallel` in all), oldest first, at a low priority behind logs not
   sent yet.
4. A resent log is judged again as on arrival. Once it is `verified` again (status `check`) the app may delete it,
   as for any log. It leaves `wanted` at the next ping.

## Refusal

An upload of a wanted name whose `Upload-SHA256` is not the SHA-256 the server stored it with (or, for a chain
gap, the `prev_sha256` the next log names) is refused before its body is kept:

```text
409 {"error": "This server lost its copy of this file and asked for it again, but this is a different file of its name.", "code": "wanted_mismatch"}
```

It is a different file under the name of a lost one: keep it, don't retry it, and don't count it as sent. When the
server never knew the SHA-256 (a sidecar of a log judged before sidecar hashes were kept), any file of the name is
taken and judged as usual.

## Resend semantics, in short

| The server | The app |
|---|---|
| lists a name in `wanted` | holds it: uploads it again, same bytes; doesn't hold it: puts it in `not_held` |
| answers `409 wanted_mismatch` | stops sending that file under that name |
| drops a name from `wanted` | nothing to do (it arrived, or is lost for good) |
