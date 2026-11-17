# Plan: moving to another server in one step

Status: design only (2026-11-17), nothing built yet. The owner's request: "Make sure there's a one click export so if
we needed to migrate to another server there's no 15 step in ssh to get it moved."

There are two ways. Both carry everything: the logs exactly as stored, the upload tokens, the trust records, the TLS
key, and later the database, places and accounts. So the CAN Bridge keeps its token and its certificate SHA-256.

- **Direct move** (the usual way). The new server pulls everything from the old one over the upload port. It catches
  up while the old one keeps taking uploads, then takes over by itself.
- **Export file**. One file with everything in it, opened on the new server with the recovery key. For when the two
  servers can't reach each other, or to keep a copy somewhere.

## What the owner does

Direct move, before the web UI exists (TrueNAS "Install via YAML", or compose):

1. **Old server: get a move code.** With a shell: `docker exec <container> /sentry-mate move code`. With no shell:
   add `SENTRY_MATE_OFFER_MOVE=yes` to its environment, redeploy, and copy the code from its log.
2. **New server: install it with empty `data` and `keys` folders and two more lines**, then start it:

   ```yaml
       environment:
         - SENTRY_MATE_MOVE_FROM=192.168.1.20
         - SENTRY_MATE_MOVE_CODE=SMMV-1K3QX-7WD2M-ZP4HT-9BN6R-XC8VA-5JE0F
   ```

   Its log shows the copy. When it says `move: finished`, the new server serves the data and the old one has
   stopped taking uploads.
3. **If the new server has another address, change it in the CAN Bridge app** (Second Sentry Six Manager, server
   address). The token and the certificate SHA-256 stay as they are. Nothing is lost meanwhile: the bridge keeps its
   logs and sends them once it reaches the new server.

Afterwards the two lines can go (they are ignored once the move is done). Keep the old server's folders a while as a
backup, then delete them. The recovery key stays the same.

Tip: if the new server will end up with the old one's address (same machine, new disk; or the address moves over),
the bridge needs no change at all.

Later, in the web UI, the same steps are buttons: Settings → Server and access → **Move to another server** shows the
code; the new server's first-run screen has **Bring my data from another server** (address and code).

Export file: `docker exec <container> /sentry-mate export > sentry-mate.smexport` on the old server. On the new one,
put the file in its data folder and run `import` (it asks for the recovery key). Later in the UI: **Download an
export**, and **Bring my data from an export file** on the first-run screen.

## Decisions

- **Bytes as stored.** Nothing is opened, decoded or sealed again: every file is copied exactly as it is on disk, at
  the same path under the data folder. Each file's sealing is bound to that path, so it opens on the new server.
- **Three kinds of file**, told apart by their first bytes: sealed whole files, growing files (vault records), plain
  files. Each kind has one copy rule. New crates' files are carried without changes here.
- **One write gate.** Writers hold a shared lock while they write. A short pause takes it exclusively. The move's last
  round and an export's snapshot each need one pause of seconds; nothing else stops.
- **The move code**: 125 random bits in 30 symbols, valid 24 hours, used once, home network only by default. It holds
  no address and no certificate (shorter to type). The claim proves the code over the TLS connection itself, so a fake
  server or someone in between learns nothing.
- **The data key travels last.** It leaves the old server only in the finish answer, wrapped with a key from the code,
  inside TLS 1.3. The key file never leaves the keys folder; the new server writes its own.
- **Finish is automatic**: once caught up, the old server pauses, the last changes are copied, the old server confirms
  and records that it moved, the new one starts. Nothing to press. The bridge's logs wait on the bridge meanwhile.
- **The new server must be new**: nothing of the owner's in its data folder. Only a first start's empty vault may be
  there, and it is replaced.
- **Export**: a snapshot made with hard links in a pause of seconds, then read while the server runs. Its bytes are the
  same every time, so a cut download resumes. It opens with the recovery key only and never holds the key file.
- **Versions**: the new server must run the same version or a newer one. An older one is refused before the code is
  used up.
- **No new dependencies**: rustls with ring (already there), hyper's client side (already there), the vault's own
  primitives. No C code.

## 1. What is in the data folder

Everything Sentry Mate keeps, today and in the crates not yet wired in:

| Path | Written by | Kind | How it changes |
|---|---|---|---|
| `vault.json` | vault | plain | The data key, wrapped twice (key file, recovery key). Written whole and renamed; changes on `restore` and `recovery-key new` only. |
| `migrated.json` | sentry-mate | sealed | Written once (the note that older plain files are sealed). |
| `tls/cert.pem` | ingest | plain | The certificate (public). Written once. |
| `tls/key.sealed` | ingest | sealed | Its private key. Written once (again if the owner brings their own `key.pem`). |
| `tokens/<sha256>.json` | `token new`, `token revoke` | sealed | Replaced whole on revoke. |
| `tokens/<sha256>.key` | the server, first upload | sealed | Written once. |
| `tokens.json` | versions before | sealed | Moved into `tokens/` when opened. |
| `can/<key>/<name>.log.xz`, `<name>.log.xz.json` | ingest | sealed | Written once, never replaced. Most of the bytes. |
| `can/<key>/counts.json` | ingest | sealed | Replaced whole after every finished log. |
| `can/<key>/uploading/<name>.part` | ingest | records | Grows while an upload arrives; removed when it is done. |
| `can/<key>/uploading/<name>.part.json` | ingest | sealed | Written once per upload. |
| `can/<key>/trust.head` | ingest | sealed | Replaced whole at each fold. The commit point of the key's trust records. |
| `can/<key>/trust-<g>.records` | ingest | sealed | Written once per generation. |
| `can/<key>/trust-<g>.journal` | ingest | records | Grows; removed after the next fold. |
| `can/.waiting/<key>`, `can/.waiting/.ready` | ingest | plain (empty) | Markers, made and removed. |
| `db/fleet.*`, `db/places.*`, `db/settings.*`, `db/kv/`, `db/cars/<id>/...` | db (not wired yet) | sealed, records | Snapshots replaced whole; journals grow; finished days packed into new files. |
| `accounts/...` | accounts (not wired yet) | sealed, records | `account-<id>.sealed`, `cars-<id>.sealed`, `policy.sealed`, `setup.sealed` replaced whole; `index.journal`, `sessions.journal` and the audit parts grow. |
| `geonames/`, `maps/*.pmtiles` | places (not wired yet) | plain | Public downloads, renamed into place. |
| `*.tmp`, `*.<hex>.tmp`, `*.sealing`, `*.to-v2`, `*.new`, `db.restoring/` | everyone | any | Temporary: a write under way, or what a crash left. |

### The three kinds and how each is copied

1. **Sealed whole files** (they start with `SMENC` 0x01). Written under a temporary name, synced, renamed into place.
   Never changed in place: a new version is a new file with a new random 16-byte file id. Copied whole. A reader can't
   see a half version: a rename replaces the name, not the file the reader has open.
2. **Growing files** (vault records: `SMPRT` 0x02, or 0x01 from older versions). Only added to. Two exceptions, and the
   copy handles both: what a failed write left at the end is cut off and written again, and the whole file is replaced
   by a new one (a fold, a rewrite in format 2, an upload started again), which has a new file id. Copied up to the end
   of the last whole record, found from the record lengths alone (no key needed). The next time only what was added
   is fetched, after two checks on the old server: the file still starts with the same 22 bytes (same file id), and
   the 16 bytes before the copy's end are still the same. Those 16 bytes are the last copied record's tag, and every
   record's tag is bound to the one before it, so this one tag stands for the whole copy. If either check fails, the
   file is copied whole again.
3. **Plain files** (anything else: `vault.json`, `cert.pem`, the markers, public downloads). Written once or renamed
   into place. Copied whole, again when the length or the time changes.

Nothing else is ever changed in place. That becomes a rule for every crate, wave 2 included: **a file is either
written whole under a temporary name and renamed, or it is a vault records file that only grows.** A crate that ever
needs something else must add a kind here first. A test walks the data folders the crates' own tests make and checks
every file is one of the three kinds.

A file's identity, for "has it changed?", is its length and its modification time (in microseconds). For growing
files it is also the file id and the last tag, as above. Two more rules make that exact:

- **Times are kept.** Each copy gets the old file's modification time. The new server then compares its copy with the
  old file by length and time alone, with no index of its own. It also keeps `clean_stale` right (it drops unfinished
  uploads two days old, by their time).
- **Racy times.** A file read less than 2 seconds after its last change (by the old server's clock) is fetched again
  in the next round. On a file system with coarse times, a second change with the same length could otherwise keep the
  same time and be missed. The last round starts 2 seconds into the pause, so nothing is racy then.

Files removed on the old server are removed from the copy.

### What is never carried

- `vault.json`. Its recovery part travels in the move's finish answer or the export's header; the new server writes
  its own with its own key file.
- Anything at the top of the data folder whose name starts with a dot: `.lock`, `.gate`, `.move/`, `.moving/`,
  `.importing/`, `.export/` (all below), and later `.backups/` (the automatic backups before an upgrade belong to the
  server that made them).
- Temporary files (the suffixes above), anywhere.
- `*.smexport` at the top of the data folder (an export file put there to import).
- Symbolic links and anything that isn't a regular file or a folder: skipped and named in the log (Sentry Mate never
  makes them).
- In the export file only: the public downloads (`geonames/`, `maps/`). They can be large, anyone can download them
  again, and leaving them out keeps every file in an export snapshot one Sentry Mate wrote itself. The new server
  downloads them again from the same source once places is wired in. The direct move does carry them.

### One consistent moment: the write gate

Files depend on each other: a trust head names the generation of its records, and the db's fleet counts the rows of
the cars' tables. Copied one by one while writes go on, they can come from different moments. That is fine while
catching up, because the new server uses nothing until the end. The last round needs one moment. So:

- `<data>/.gate` is an empty file. Every write to the data folder holds a shared lock on it (`flock`) while it writes:
  an upload chunk with its trust record and its token's first tie, the sweep, the clean-up, a token made from the
  command line; later every db, accounts and places write. Long jobs (a rebuild) hold it per step, never for the whole
  job. Each writer opens the file for its own lock.
- A **pause** takes it exclusively. It waits for writes under way (at most 60 s, else the pause fails and is tried
  again later), and new writes wait. Upload requests wait at most 20 s, then answer `503 moving` with
  `Retry-After: 60`; the bridge tries again at its next pass.
- Pauses are short: the move's last round (seconds, about a minute at most) and an export's snapshot (about 10 µs a
  file: some 10 seconds for a million files).
- `<data>/.lock` is held exclusively by the running server its whole life. A second server can't open the same data
  folder (two containers on one volume), and the command line can tell whether a server runs.
- Both are plain `flock` locks on a local file system (ZFS, ext4, xfs, btrfs). A data folder on a network share
  without locks isn't supported (that was already so).

## 2. The direct move

### 2.1 The move code

```text
SMMV-1K3QX-7WD2M-ZP4HT-9BN6R-XC8VA-5JE0F        (made up)
```

30 Crockford base32 symbols in six groups of five, the recovery key's alphabet (no I, L, O or U; `O` reads as 0, `I`
and `L` as 1). Case and spaces don't matter:

- 1 symbol: the code's format (1).
- 25 symbols: the secret, 125 random bits.
- 4 symbols: a 20-bit check (from SHA-256 of the rest), so a typo is found before anything is sent.

It has no address and no certificate SHA-256 in it. The owner gives the address beside it (the UI asks for both), and
the certificate is learned in the claim, proven by the code itself (2.3). That keeps the code short enough to type.

From the secret, with HKDF-SHA256 (salt `sentry-mate move code v1`):

- the **auth key** (`auth`): proves the code in the claim;
- the **wrap key** (`wrap`): wraps the data key in the finish answer;
- the **code id** (`id`, 8 hex digits): names the code in logs and status lines. The code itself is never logged,
  except when the owner asked for it with `SENTRY_MATE_OFFER_MOVE`.

Rules:

- Made by `move code`, or the UI (admin, password and two-step code again), or by `SENTRY_MATE_OFFER_MOVE` on start.
- Valid **24 hours**. Used **once**: the first claim takes it. **One at a time**: a new code ends the one before and
  any move not finished. `move cancel` revokes it.
- **Home network only** by default (loopback, private IPv4, link-local, IPv6 unique local: the rule of
  `accounts::net`). `move code --anywhere` (or `SENTRY_MATE_OFFER_MOVE=anywhere`) for a new server elsewhere, such as
  a rented server. This is a second fence; the code is the real one.
- Kept on the old server in `.move/code.sealed`: the auth key's SHA-256, the wrap key, the code id, when it expires.
  Never the code.
- **Rate limits**: 10 wrong tries from one address (IPv6: its /64) in 10 minutes, then `429 slow_down`. 100 wrong
  tries in all, and the code stops working (the log says so; make a new one).
- The code in the log (`SENTRY_MATE_OFFER_MOVE`): with no shell, the log is the only screen the owner has. That is
  acceptable because the code works once, for 24 hours, and gives the data key only at the end of a whole move, which
  stops the old server and says where the data went. The log line says not to share it.

### 2.2 The protocol on port 10100

The move uses the upload port: the same TLS 1.3 server, the same connection guards (6 connections per address, a
request at most 10 minutes). Its paths are under `/api/move/v1/`. With no code made and no move under way, every one of
them answers `404 not_found` like any unknown path, so a scan learns nothing. Errors have the upload API's shape:
`{ "error": "<words>", "code": "<code>" }`.

| Path | Method | Proof | Body and answer |
|---|---|---|---|
| `/api/move/v1/claim` | POST | the code, bound to the connection (2.3) | `{ v: 1, claim, app, proof }` -> `{ session, server_proof, app, cert_sha256, files, bytes }` |
| `/api/move/v1/list` | GET | session | `?after=<path>&limit=` (at most 20,000) -> lines of JSON, one per file, then `{ next, now_us, paused }` |
| `/api/move/v1/fetch` | POST | session | `{ want: [{ p, from, to, id?, tail? }] }` (at most 4,096) -> frames of bytes, at most 64 MiB |
| `/api/move/v1/pause` | POST | session | -> `{ pause, until_ms }` once writes have stopped |
| `/api/move/v1/finish` | POST | session | `{ pause, files, bytes, digest }` -> `{ moved_ms, key_id, data_key, recovery }` |
| `/api/move/v1/status` | GET | session | -> `{ state: "copying" \| "paused" \| "moved", pause, moved_ms }` |
| `/api/move/v1/session` | DELETE | session | the new server gives up: the session ends, a pause ends |

A list line: `{ "p": "can/abc123/abc123_20261108T143000Z_1.log.xz", "k": "s", "len": 233006, "mt": 1794148261123456 }`.
`k` is `s` (sealed whole), `r` (records) or `p` (plain). A records line also has `id` (its file id, hex) and `len` is
the end of its last whole record. Paths are in **walk order**: depth first, each folder's entries sorted by name
bytes. Both servers walk the same way, so the new one compares the list with its copy in one pass. About 110 bytes per
file: a million files make about 110 MB per round, a second or two on a home network.

A fetch answer is binary:

```text
frame = 0x01 | varint path length | path | u64 from | u64 count | u64 length now | u64 time (µs) | u64 read at (µs)
        | flags (u8) | the bytes
        flags: 1 changed (the file id or the tail didn't match: no bytes; it is fetched whole next round), 2 gone
end   = 0x00 | varint frames
```

Wants a 64 MiB answer can't hold are left out and asked for again. So a request stays far inside the 10-minute limit
even on a slow link, and big files come in pieces that can be resumed.

Session errors: `401 bad_session`, `410 cancelled` (the owner cancelled on the old server), `409 not_paused` (a
finish without a pause in force), `409 digest_mismatch`.

### 2.3 Claim: proving the code without giving it away

- Both ends take a **TLS exporter** value from their connection (RFC 8446 section 7.5, label
  `EXPORTER-sentry-mate-move-v1`, 32 bytes). It is the same at both ends of one TLS connection and different on any
  other. Someone in between has to run two connections, one to each side, so a proof made on one is worthless on the
  other.
- The new server sends `proof = HMAC-SHA256(auth key, "sentry-mate move v1 client" | exporter)`. For this one request
  it accepts any certificate (TLS still checks that the server holds its certificate's key). It also sends `claim`, 16
  random bytes it saved first, and its version (`app`).
- The old server checks the proof in constant time, then the version (section 7): an older new server is
  refused with `409 too_old` and the code stays usable. Then the code is taken. The answer carries the session (256
  random bits; the old server keeps only its SHA-256), its own version, its certificate's SHA-256, and the files and
  bytes to copy, with `server_proof = HMAC-SHA256(auth key, "sentry-mate move v1 server" | exporter | session | app |
  cert_sha256 | files | bytes)`.
- The new server checks `server_proof` before it uses anything. From then on it pins the old certificate by that
  SHA-256 on every connection and sends `Authorization: Bearer <session>`.
- A fake server, or someone in between, sees one HMAC over a value of its own connection. With a 125-bit secret it
  can't be guessed offline. The secret and the keys never cross the network. The new server tells the owner: "the
  server at 192.168.1.20 couldn't prove it made this code: nothing was sent".
- A lost answer: the same code with the same `claim` gets the same session again. Any other claim gets
  `409 code_used` with when and from which address it was used.
- The session lasts until the move ends, or 7 days with nothing from the new server (then the old server drops it and
  says so).
- A proxy that ends TLS in front of the upload port would break the bridge's pin anyway. Pass-through (TCP) proxies
  are fine.

### 2.4 Copy rounds

- **Round 1** lists everything and fetches everything. The new server checks the free space first: the total from the
  claim plus 2 GiB (ingest's `MIN_FREE`). With less, it waits and says how much is missing.
- **Later rounds** list again. The new server compares each line with its copy: missing or different length or time:
  fetched whole (a growing file of the same id: only what was added); fetched racy last time: fetched again; on its
  side but not in the list: removed.
- Four connections fetch at once (the server allows six per address).
- Writes on the new side are synced as they land. A round can stop anywhere (a cut network, a restart of either
  server): the next round fetches only what is still missing.
- The new server checks what it writes: a sealed file starts with `SMENC` and has a valid sealed length, and the
  bytes fetched for a growing file are whole records.
- Rounds go on until one finds at most 64 MiB and 1,000 files to fetch (or after 10 rounds). Then the last round.

About 180 GB and a million files take about half an hour on gigabit; the bridge's uploads (under a megabyte a minute
per car) make each catch-up round small.

### 2.5 Finish (automatic)

1. **Pause.** The new server asks for the pause. The old server takes the gate exclusively (waiting for writes under
   way), waits 2 seconds (so file times settle), and answers with a pause id. Uploads wait, then get `503 moving`.
2. **Last round.** The new server lists and fetches what changed since the round before. Nothing is writing now, so
   this round is exact.
3. **Finish.** The new server sends the number of files, their bytes and a digest: SHA-256 over
   `path \t kind \t length \t time \n` for every file it holds, in walk order.
4. **The old server checks.** It walks itself the same way, under the pause, and compares. Different: `409
   digest_mismatch` with its own count; the new server does another round, and the pause goes on. The same: it writes
   `.move/moved.sealed` (synced: this is the commit point), then answers with:
   - `data_key`: the data key sealed with the wrap key (XChaCha20-Poly1305, a random nonce, associated data
     `sentry-mate move v1: data key` | key id | session);
   - `key_id`, and `recovery`: the recovery part of its `vault.json`, unchanged;
   - `moved_ms`.
   The pause becomes the moved state (2.6).
5. **The new server** opens the data key, checks its key id, switches (3.3), and starts serving.

- The pause ends by itself when the new server sends nothing for 60 s, or after 15 minutes in all. The old server then
  takes uploads again, and the new one catches up and asks again.
- A lost finish answer: the new server asks again and gets the same answer (rebuilt from `moved.sealed` and the
  session, a new nonce), also after a restart of the old server.
- Why the key comes last: someone who got hold of the code gets nothing they can read unless they finish a whole move,
  and finishing stops the old server and makes it say where the data went.

### 2.6 The old server after the move

- `.move/moved.sealed` says when, to which address, which session and code id. It stays until undone. It is never
  carried (it is in a dot folder).
- Every upload path answers `503 moved` with `Retry-After: 3600` and "This server's data moved to another server: change
  the server address in the CAN Bridge app." The bridge shows "answered 503 moved; will retry" and keeps its logs.
- It writes nothing more: no housekeeping, and commands that write refuse ("this server's data moved to 192.168.1.30 on
  2026-11-17 at 14:02"). Reading still works (`log export`, `chain`, `fingerprint`, an export file; later the UI, read
  only, with a banner).
- Once an hour while a bridge still calls: "a CAN Bridge (car key abc123) still sends here, last at 15:10: change its
  server address to the new server". The bridge keeps logs for weeks (its folder holds 40 GB by default; with two
  servers the oldest roll over once it is full, those Sentry Six Manager already has first).
- **Undo**: `move undo` (it asks to type `yes`) or the UI: the server takes uploads again. If the new server also takes
  uploads, the two drift apart, so the warning says to stop the new one first. Undo goes in the audit log. The old
  server's data was never changed by the move, so undo needs nothing else.

### 2.7 What the bridge sees

| When | Old server | New server |
|---|---|---|
| Copying | Takes uploads as usual | Upload port closed: "not reachable; will retry" |
| The last pause (seconds) | Waits, then `503 moving`: "will retry" | Closed |
| After the move | `503 moved`: "will retry" | Serves, with the same certificate, tokens and unfinished uploads |

After the address change, `status` finds every file the old server had, and unfinished uploads go on from where they
were. The bridge sends nothing twice and loses nothing.

## 3. The new server's side

### 3.1 New means unused

The new server takes a move (or an import) only into a data folder with nothing of the owner's in it: no CAN logs, no
uploads in progress, no tokens, and once they are wired in, no cars, accounts or imports.

- What a first start makes by itself doesn't count (`vault.json`, `tls/`, `migrated.json`, empty folders, markers),
  and neither does an export file put at the top of the data folder to import. So a server started once without the
  two lines can still take a move: its own empty vault is set aside and removed at the switch, with the
  `RECOVERY-KEY.txt` it made (that key would open nothing; the log says so).
- The keys folder must be empty, or hold only that first start's key file. A key file that opens nothing in this data
  folder is refused ("give the new server an empty keys folder"): it may belong to data elsewhere.
- A data folder in use is never touched. With the two lines set, the server starts as usual and warns: "nothing was
  moved: this server already has data; remove SENTRY_MATE_MOVE_FROM and SENTRY_MATE_MOVE_CODE".

### 3.2 During the move

- Everything copied goes to `.moving/data/`, at the same paths, times kept. A half-moved folder never looks like a data
  folder.
- At the claim the new server writes **its own new key file**, `<keys>/.moving/sentry-mate.key` (32 random bytes), and
  seals its move state with it in `.moving/state.sealed`: the address, the old certificate's SHA-256, the session, the
  claim bytes, the keys from the code, the round, the progress. A restart reads it and goes on. The old server's key
  file never leaves its keys folder.
- The upload port stays closed until the switch. `sentry-mate health` answers healthy while a move runs, so Docker and
  TrueNAS don't mark the app as failing.

### 3.3 The switch

Every step can run again after a crash: on start, `.moving/` is looked at before anything else, before the vault is
opened or made.

1. The state says `switching`.
2. A first start's files, if there are any, are set aside in `.moving/replaced/`.
3. Each entry at the top of `.moving/data/` is renamed into the data folder (one file system: instant at any size).
4. `vault.json` is written: the data key wrapped by the new key file, and the old server's recovery part as it was.
   **The old recovery key opens the data here too.**
5. `<keys>/.moving/sentry-mate.key` is renamed to `<keys>/sentry-mate.key`.
6. `.move/moved-in.sealed` is written (from where, when, the code id), and `.moving/` is removed.
7. The server starts as usual: the vault, the TLS key, the tokens, then the upload port.

### 3.4 Environment variables

| Variable | Server | What it does |
|---|---|---|
| `SENTRY_MATE_MOVE_FROM` | new | The old server: `192.168.1.20`, `nas.local:10100`, or the bridge's whole upload address (`https://192.168.1.20:10100/api/can/logs`). Port 10100 unless given. |
| `SENTRY_MATE_MOVE_CODE` | new | The move code. |
| `SENTRY_MATE_OFFER_MOVE` | old | `yes`: keep a move code ready (home network only) and print it in the log, and a new one when it expires. `anywhere`: the same, for a new server anywhere. Ignored once the server has moved. |

On start:

- `.moving/` is there: carry on with that move (the two lines may be gone by now; what is needed is in the sealed state).
- The data folder is unused and both lines are set: start the move.
- This server already took a move with that code: one line, "the move from 192.168.1.20 is done; you can remove the
  two lines".
- The data folder is in use: start as usual and warn (3.1).

The new server never exits over a move problem, because Docker would restart it in a loop. It waits, says why in the
log, and tries again where that can help (an unreachable server, a full disk). Where it can't (a wrong, used or expired
code), it waits for the owner: a new code in the lines and a restart, or `move cancel`.

### 3.5 What the log says

The new server, a whole move (the old server's address, sizes and times are examples):

```text
move: bringing the data from 192.168.1.20:10100 (move code 3f9a1c07)
move: the old server proved the code; it runs Sentry Mate 0.4.1, this one 0.4.2
move: 1,234,567 files, 182.4 GB to copy; 912.0 GB free here
move: copying: 5 % (9.1 of 182.4 GB, 61,800 of 1,234,567 files), about 28 min left
move: copying: 50 % (91.2 of 182.4 GB, 617,000 of 1,234,567 files), about 15 min left
move: first copy done in 31 min; catching up with what changed meanwhile (34 files, 3.1 MB)
move: caught up; the old server pauses uploads for the last changes
move: last changes copied (12 files, 640 KB) in 4 s; the old server confirmed
move: finished: this server holds the data now and serves it. The old server stopped taking uploads at 14:02.
move: in the CAN Bridge app, set the server address to https://<this server>:10100/api/can/logs; the token and the
      certificate SHA-256 stay as they are (a1b2c3d4…)
move: the old server's recovery key opens the data here too. You can remove SENTRY_MATE_MOVE_FROM and
      SENTRY_MATE_MOVE_CODE now.
CAN Bridge uploads on https://0.0.0.0:10100/api/can/logs (TLS 1.3), certificate SHA-256 a1b2c3d4…, vault 7c1e…
```

When something is wrong:

```text
move: the move code has a typo: check it against the one the old server showed
move: nothing answers at 192.168.1.20:10100 (connection refused); trying again in 30 s
move: there is no move waiting at 192.168.1.20:10100: make a move code there (update it first if it has no
      `move code` command)
move: the old server didn't take this code: check it, or make a new one there
move: this move code expired at 2026-11-18 14:05: make a new one on the old server, put it in SENTRY_MATE_MOVE_CODE
      and restart this server
move: this move code was used already, by 192.168.1.77 at 13:40. If that wasn't this server, run `move cancel` on
      the old server now.
move: the server at 192.168.1.20 couldn't prove it made this code: it isn't the old server, or something in between
      changed the connection. Nothing was sent.
move: the old server runs a newer Sentry Mate (0.5.0) than this one (0.4.2): update this server first, then start it
      again (the code still works)
move: not enough space: 182.4 GB plus 2 GB to spare are needed, 120.0 GB are free. It carries on by itself when there
      is room (checked every minute); to give up: `move cancel`
move: lost the connection to the old server; trying again in 10 s (it carries on where it stopped)
move: the old server restarted; carrying on
move: the move was cancelled on the old server; what was copied here is removed. This server is empty again.
```

The old server:

```text
move: a move code was made (3f9a1c07), valid until 2026-11-18 14:05, works once, from this home network only
move: a server at 192.168.1.30 took the move code (Sentry Mate 0.4.2) and is copying the data
move: 192.168.1.30 has copied 50 % (91.2 of 182.4 GB)
move: uploads paused for the last changes (192.168.1.30 is catching up)
move: done: the data moved to 192.168.1.30 at 14:02. This server no longer takes uploads and tells the CAN Bridge
      to try later. Keep its folders as a backup for a while, then delete them. To take uploads here again:
      `move undo`.
```

## 4. The export file

### 4.1 Format

```text
export = "SMEX" 0x01 | u32 header length (big endian) | header (JSON) | body (sealed)

header = { "format": 1, "app": "0.4.1", "id": "<32 hex>", "made_ms": 1794924000000, "key_id": "<16 hex>",
           "recovery": { "salt", "kdf": { "m", "t", "p" }, "nonce", "ct" }, "files": 1234567, "bytes": 195833364480 }

body   = the vault's sealed-file format ("SMENC" 0x01 | file id | 64 KiB chunks, each with its tag, the last one
         flagged), file id = the export id, label "sentry-mate export v1 " + hex SHA-256 of everything before the body

plain  = "SMXB" | u16 format
         | varint count | count x ( varint path length | path | kind (u8) | u64 length | u64 time (µs) )   the manifest
         | every file's bytes, one after the other, in manifest order
         | "SMXE" | varint count
```

- **It opens with the recovery key.** The header carries the recovery part of `vault.json` (salt, Argon2id settings,
  the wrapped data key). The key file is never in it, and neither is the data key, so the file alone is useless to
  someone who takes it: they would need the recovery key (256 random bits through Argon2id at 1 GiB).
- **All of it is authenticated.** The body is sealed with a key from the data key, chunk by chunk, so a changed, cut,
  reordered or swapped chunk doesn't open. The label binds the header (a header from another export, or an edited one,
  doesn't fit the body). The manifest is inside the sealing. File names and sizes are hidden too.
- **Sealed twice over.** The files inside are still sealed as they were stored; the body seals them again. That costs
  one more pass of XChaCha20-Poly1305, quick next to reading the disk, and hides the names.
- **The manifest comes first**, so an import knows the paths, kinds and sizes before it writes anything.
- An export opens with the recovery key that was in use when it was made. After `recovery-key new`, older exports
  still need the old key; the CLI and the UI say so.
- File name: `sentry-mate-<date>.smexport`.

### 4.2 Written while the server runs: a snapshot

1. A pause (some 10 seconds for a million files).
2. Every sealed whole file to carry is **hard-linked** into `.export/<id>/files/`, at the same paths. That is instant
   and takes no space. It is a true snapshot because the server never changes those files in place: it replaces them,
   and a link keeps the version it points to.
3. **Growing files and plain files are copied** there instead (growing files up to their last whole record), because
   growing files are added to in place, and a plain file could be written over by something else. They are small:
   journals, uploads in progress, the certificate and the markers (the public downloads aren't in an export).
4. The plan is sealed into `.export/<id>/plan.sealed`: the export id, the header, each file's path, kind, length and
   time.
5. The pause ends. The export is read from the snapshot while the server takes uploads as usual.

- Hard links need the data folder on one Linux file system: ZFS (TrueNAS), ext4, xfs and btrfs are all fine. Where
  links can't be made, the export is refused with that reason; the direct move still works.
- One snapshot at a time; a new export replaces it. It is removed 24 hours after its last download request, after 7
  days at most, by `DELETE`, or when a command-line export ends. One a crash left is removed at the next start. While
  it is there, files deleted from the data folder keep their space (logs are rarely deleted).

### 4.3 Downloading and resuming

- The snapshot can't change, so the export's bytes are the same every time it is read. Its length is known from the
  plan before the first byte, so the download has a size and the browser shows time left.
- A cut download resumes with a `Range` request (the `ETag` is the export id). The server finds the chunk holding the
  first byte asked for, reads that chunk's plaintext from the snapshot, and seals on from there with the same key and
  the same chunk numbers. Same key and nonce only ever seal the same bytes, because the snapshot can't change.
- The command-line export writes to standard output in one go (no resume needed for a local file). It refuses to write
  to a terminal.

### 4.4 Import

Into a new, unused server (3.1) only. Nothing in the data folder is touched until everything has been checked:

1. **Header**: the magic, a known format, an app version no newer than this one (else: "made by Sentry Mate 0.5.0;
   update this server first").
2. **Recovery key**: Argon2id, then the data key, then its key id must match the header's. Wrong: refused, nothing
   written.
3. **The target is unused** (3.1). Else refused.
4. **The body** is opened as it is read; every chunk is checked.
5. **The manifest** first: every path relative, at most 1,024 bytes, letters, digits and `._-` only, no `.` or `..`
   parts, nothing that wouldn't be carried (a dot entry at the top, a temporary name), no path twice, in walk order;
   every kind known. Then the space: the total plus 2 GiB must be free, or it stops before writing a file.
6. **Files** go into `.importing/data/` (times kept, synced). Each must start the way its kind does.
7. **The end**: the end marker, the count, and the vault's flag on the last chunk. A file cut short fails here.
8. **The switch**, as in 3.3: a new key file, `vault.json` with the export's recovery part, the files renamed into
   place.

Any failure before the switch removes `.importing/` and leaves the data folder as it was. The importer streams: it
holds the manifest's current entry, not the whole list.

From the UI the recovery key and the file's first bytes (the header) are sent first, so a wrong key is found before a
large upload. The chunks are then opened as they arrive, so the upload is never stored twice.

## 5. Commands

Run with `docker exec <container> /sentry-mate ...` (or `docker compose exec sentry-mate /sentry-mate ...`):

| Command | Server | What it does |
|---|---|---|
| `move code [--anywhere]` | old | Makes the move code and shows it once, with when it expires. Replaces an earlier code and ends a move not finished. |
| `move status` | either | What the move is doing: the code waiting, who is copying, how far, paused, moved (and when, to where). |
| `move cancel` | either | Old: the code stops working, a move not finished ends, a pause ends. New: the move stops and what was copied is removed (the old server is told). |
| `move undo` | old | After a move: take uploads here again. Asks to type `yes` and warns about the new server. |
| `move from ADDRESS CODE` | new | Brings the data here. Shows the progress; the server carries on if the command is closed. With no server running (`docker compose run --rm`), it does the whole move and exits. |
| `export` | any | The whole server as one export file, to standard output. With `docker compose exec`, add `-T` so the bytes aren't changed by a terminal: `docker compose exec -T sentry-mate /sentry-mate export > sentry-mate.smexport`. |
| `import FILE` | new | Brings an export file in (put it in the data folder first, e.g. `/data/sentry-mate-2026-11-17.smexport`). Asks for the recovery key, like `restore` (so `docker exec -it`). |

`move code` prints:

```text
Move code (shown once; valid until 2026-11-18 14:05; works once; from this home network only):

  SMMV-1K3QX-7WD2M-ZP4HT-9BN6R-XC8VA-5JE0F

On the new server (empty data and keys folders), set:
  SENTRY_MATE_MOVE_FROM=<this server's address: the one the CAN Bridge uses>
  SENTRY_MATE_MOVE_CODE=<the code above>
or run there: sentry-mate move from <this server's address> <the code above>
```

How the commands reach the running server: through files, as `token revoke` already does. A command writes a sealed
request in `.move/` and the server looks for requests every second. A command that moves data in or imports on a running new server checks what it can itself (the
code's typo check; the export header and the recovery key), then hands the work to the server, sealed with that first
start's vault. `export` takes the write gate itself for the snapshot, so it needs nothing from the server. Commands
that write hold the gate's shared lock like the server's writers.

## 6. API for the UI (`/api/v1`)

For the web server (wave 2) and the UI packs; to be merged into `docs/ui-api.md` when they are built. The conventions
there apply (one error shape, CSRF on every change, audit log). Changes here are admin only and ask for the password
(and the two-step code when it is on) again; the first-run ones take the one-time set-up code instead. Pages: Settings: Server and access (old server), First-run set-up (new
server), Settings: Server and access (exports).

| Resource | Methods | Fields and notes | Pages |
|---|---|---|---|
| `/api/v1/move` | GET (admin) | `{ role: "none" \| "old" \| "new", state: "none" \| "offered" \| "copying" \| "paused" \| "moved" \| "cancelled", code: { id, expires_ms, anywhere } \| null, peer: { address, app, claimed_ms, last_seen_ms } \| null, progress, moved: { at_ms, to_address } \| null, problem: { code, message, retry_ms } \| null }` | Settings: Server and access |
| `/api/v1/move/code` | POST, DELETE (admin) | POST `{ password, two_step, anywhere }` -> `201 { code, id, expires_ms, anywhere }`, the code shown once; it replaces an earlier code and ends a move not finished. DELETE revokes it. | Settings: Server and access |
| `/api/v1/move/cancel` | POST (admin) | Ends a move not finished. `409 { code: "finished" }` after a move: use undo. | Settings: Server and access |
| `/api/v1/move/undo` | POST (admin) | `{ password, two_step, confirm: true }`. After a move: this server takes uploads again. | Settings: Server and access |
| `/api/v1/move/events` | GET (server-sent events, admin) | Events below. | Settings: Server and access |
| `/api/v1/setup/move` | POST, GET, DELETE | First run, no account yet: the one-time set-up code stands in for a session. POST `{ setup_code, address, code }` -> `202 { watch }`. `400` with `fields` (`address`, `code`: a typo), `409 not_empty`. GET and DELETE take `?watch=`; GET has the shape of `/api/v1/move`, DELETE cancels. | First-run set-up |
| `/api/v1/setup/move/events` | GET (server-sent events) | `?watch=`. The same events. After `done` the server serves the moved data; the accounts are the old server's, so the person signs in with their old account. | First-run set-up |
| `/api/v1/exports` | POST, GET (admin) | POST `{ password, two_step }` -> `202 { id, state: "preparing" }`; the snapshot needs a pause of seconds (`503 busy` with `Retry-After` when a pause can't be had now). GET lists the snapshot there is (at most one). | Settings: Server and access |
| `/api/v1/exports/{export}` | GET, DELETE (admin) | `{ id, state: "preparing" \| "ready" \| "sending" \| "sent" \| "expired" \| "failed", file_name, files, bytes, sent_bytes, made_ms, expires_ms, url }`. DELETE removes the snapshot. | Settings: Server and access |
| `/api/v1/exports/{export}/file` | GET (admin) | The file: `application/octet-stream`, `Content-Disposition: attachment; filename="sentry-mate-2026-11-17.smexport"`, `Content-Length`, `ETag`, `Accept-Ranges: bytes`. `Range` (and `If-Range`) -> `206`. A snapshot no longer there -> `404 export_gone` (make a new one). A plain link works: the session cookie goes with it. | Settings: Server and access |
| `/api/v1/exports/events` | GET (server-sent events, admin) | `state`, `progress` with `{ id, sent_bytes, bytes }`. | Settings: Server and access |
| `/api/v1/setup/import` | POST | First run: `{ setup_code, recovery_key, header }` (`header`: the file's first 64 KiB, base64) -> `201 { id, watch, files, bytes, made_ms, app }`. `422 not_an_export`, `422 wrong_recovery_key`, `422 newer_version`, `409 not_empty`, `507 no_space`. Throttled like sign-in. | First-run set-up |
| `/api/v1/setup/import/{import}` | PATCH, GET, DELETE | `?watch=`. PATCH: one chunk (at most 16 MiB) with `Upload-Offset` and `Upload-Length` (no `Upload-SHA256`: the file checks itself) -> `{ received }`, `409 offset_mismatch { received }` to resume. GET: `{ state: "uploading" \| "checking" \| "switching" \| "done" \| "failed", received, bytes, files_done, files, problem }`. DELETE discards it. | First-run set-up |
| `/api/v1/setup/import/{import}/events` | GET (server-sent events) | `?watch=`. `state`, `progress`, `problem`, `done`. | First-run set-up |

Events (every stream above):

| Event | Data |
|---|---|
| `state` | `{ state, at_ms }`. New server: `connecting`, `copying`, `catching_up`, `finishing`, `switching`, `done`, `waiting` (the problem says why), `failed`. Old server: `offered`, `copying`, `paused`, `moved`, `cancelled`. |
| `progress` | `{ round, files_done, files_total, bytes_done, bytes_total, bytes_per_s, eta_s }`, at most once a second. |
| `problem` | `{ code, message, retry_ms }`; `code` one of `unreachable`, `no_move`, `wrong_code`, `code_used`, `code_expired`, `not_proven`, `too_old`, `not_empty`, `no_space`, `cancelled`. `retry_ms` null when it waits for the person. |
| `done` | `{ at_ms, fingerprint, upload_address_hint }` |

Also:

- `GET /api/v1/setup` gains `moving` and `importing` (true while one runs), so the first-run screen shows the progress
  page after a reload.
- `/api/v1/notices` gains `move_code_waiting` (a code is waiting until ...), `moving`, `moved` (this server's data
  moved to ... on ...; it takes no uploads) and `bridge_still_sending`.
- The audit log records: a code made or revoked, claimed (from where), the pause, the finish, a cancel, an undo, an
  export made and downloaded, an import done, a move received.

What the pages show: the old server's page shows the code large with a copy button and its expiry, then "Waiting for
the new server", then a progress bar, then "Moved to 192.168.1.30 on ..." with **Take uploads here again**. The
first-run screen asks for the address and the code, shows the progress, then "Done: sign in with your account from
the old server", and the bridge note.

## 7. Versions

- **The new server must run the same version as the old one, or a newer one.** An older version may not read newer
  formats (versions before records format 2 can't read format 2 files). The move checks this in the claim, before the
  code is used up; the import checks the header's `app`. Both say which side to update.
- **A newer new server** takes the old formats as they are, because the copy never looks inside files. Its normal start
  then brings them up to date, the same as pulling a newer image. The old server keeps its own copy in its own format,
  so undo still works.
- **The move protocol** has its version in the path (`/api/move/v1`). A later v2 keeps v1 while a supported version
  still speaks only v1. The new server is never older than the old one, so it is always the side that knows both, and
  it speaks what the old one knows.
- **The code** starts with its format symbol. A code of a format this server doesn't know: "made by a newer Sentry
  Mate: update this server".
- **The export** has `format` in its header and inside the body. Readers for every older export format are kept; a
  newer one is refused with its version.
- **New kinds of file** (a crate that must change a file in place) need a new copy rule, a new move protocol version and
  a new export format. The test of section 1 catches a crate that forgets.

## 8. When things go wrong

| Case | What happens | What the owner sees |
|---|---|---|
| A typo in the code | Found by the code's check before anything is sent | "the move code has a typo" |
| Wrong code | `403 wrong_code`; counted for the rate limit. The new server doesn't try again (that would use up tries) | "the old server didn't take this code" |
| Code expired | `410 code_expired` | When it expired, and how to make a new one |
| Code used by someone else | `409 code_used` with when and from where | That line, and "run `move cancel` on the old server now" |
| The code tried on the wrong server | That server has no move: `404`; or it has another code: `wrong_code` (and counts it). It learns nothing | "there is no move waiting at ..." or "didn't take this code" |
| A fake server or someone in between | The proofs don't match across two connections; nothing is sent | "couldn't prove it made this code. Nothing was sent." |
| New server older than the old | `409 too_old` in the claim; the code stays usable | Which side to update |
| Network cut | Every request is tried again (1 s, doubling, at most 60 s), forever; nothing is fetched twice | "lost the connection; trying again" |
| Old server restarted | The session is kept (sealed). A pause is gone: the new server catches up and asks again. The moved state is kept | "the old server restarted; carrying on" |
| New server restarted | Carries on from `.moving/`: files already there are not fetched again | "carrying on the move from 192.168.1.20 (45 % was copied)" |
| Disk full on the new server | Checked before round 1 (total plus 2 GiB); a full disk later pauses the copy, checked every minute | How much is needed and free, and how to give up |
| Disk full on the old server | Its own uploads fail as before (`507 no_space`). If it can't write the moved state, the finish fails, the pause ends by itself, the new server tries again | The old server's log says the disk is full |
| The new server goes away mid-move | The old server keeps serving; a pause ends after 60 s; the session is dropped after 7 days | `move status` on the old: "a move to 192.168.1.30 started ..., not heard from since ..." |
| Cancel on the old server while the new one finishes | One lock: whichever comes first wins. Cancel first: the new server gets `410` and removes its copy. Finish first: cancel says "finished; use `move undo`" | One of those two lines |
| Both servers taking uploads | Can't happen through the move: the old one stops at the pause, the new one starts only after the old one wrote its moved state. Only `move undo` with the new one still running does it | The undo warning |
| A second server on the same data folder | `.lock` is held | "another Sentry Mate uses this data folder" |
| A file changed in place by something outside Sentry Mate | The digest of the last round catches a different length or time | `digest_mismatch`, then another round |
| Export download cut | Resumes with `Range` while the snapshot is there; after that, `404 export_gone` | The browser resumes; or "make a new export" |
| Import: wrong recovery key | Refused before anything is written | "that recovery key doesn't open this file" |
| Import: file damaged or cut | Stops at the first bad chunk; `.importing/` removed | "the file is damaged or cut short at byte N: make the export again" |
| Import: not enough space | Found from the manifest before a file is written | How much is needed and free |
| Import or move into a server in use | Refused; nothing touched | "this server already has data" |

## 9. Tests

Unit (in the new crate, and in `vault` and `ingest` for their parts):

- Code: made, written and read back; every single-symbol typo found by the check; `O`/`I`/`L` mix-ups read right; wrong
  length or format refused; the code id doesn't give the code away.
- Claim: the right code on one connection is taken; a proof passed from one connection to another is refused; a wrong
  code counts, 10 from one address give `429`, 100 in all end the code; an expired, used or cancelled code is refused;
  the same `claim` gets the same session, another gets `code_used`; an older new server is refused and the code still
  works; the server proof is checked before anything is used.
- Copy rules: a sealed file copied whole and again only when its length or time changes; a racy one fetched again; a
  growing file copied to its last whole record (a torn last record left out), then only what was added; a growing file
  whose failed write was cut off and written again is copied whole again; one replaced (new file id) copied whole; a
  removed file removed; temporary files, dot entries, links and `vault.json` never carried; times kept.
- Consistency: trust folds, uploads finishing and tokens made during the rounds; after the last round under the pause,
  the copy opens and gives the same token list, chain reports and log counts as the old server.
- The gate: a pause waits for a write under way, holds new ones, gives up after 60 s; uploads wait 20 s then `503
  moving`; the pause ends after 60 s of silence and after 15 minutes; `.lock` stops a second server.
- Finish: a digest that doesn't match gives another round; the moved state is written before the answer; a lost answer
  gets the same key; the wrapped key opens only with the code's wrap key and that session.
- The new side: the unused rule (a first start's vault is taken and replaced; one log, token or upload in progress
  refuses); a keys folder with a key file that opens nothing is refused; a stop at every step of the switch, then a
  start, ends in the same data folder; the env rules of 3.4.
- Old server moved: uploads `503 moved`, writes refused, the hourly line, undo, all kept across a restart.
- Export: made, then imported into an empty folder: every file the same bytes and times, and the token list, chain
  reports and (once wired) the db and accounts the same; made while uploads go on (the snapshot is one moment); read
  again and again, and from a thousand random `Range` starts, always the same bytes; one flipped bit anywhere, a cut
  anywhere, a header from another export, a wrong recovery key: refused with nothing left behind; a path with `..`, an
  absolute path, a dot entry or a path twice in a made-up manifest: refused; not enough space found from the manifest;
  a newer `app` refused; links impossible: refused with the reason.

In one process (two servers on loopback, each with its own folders):

- A whole move while a fake bridge uploads every second to the old server, then to the new one: every log arrives once,
  nothing is lost or doubled, the bridge only ever sees `200`, `201` and `503`, unfinished uploads carry on.
- A proxy between them that cuts connections at random; the old server stopped and started mid-copy and during the
  pause; the new one killed at every step and started again.
- Versions both ways (new older: refused and the code kept; new newer: moved).

**Two real servers in Docker** (a script, run before a release and by hand; it needs Docker and the image built from
the tree):

1. A Docker network; the old container runs as user 568:568 (TrueNAS) with a read-only root, `cap_drop: [ALL]` and
   `no-new-privileges`, as the compose file does. Make two tokens with `docker exec`.
2. A small test uploader (built from the test helpers, synthetic signed logs, made-up keys) sends a log every few
   seconds for two cars, all through the test.
3. `docker exec old /sentry-mate move code` gives the code. The new container starts with empty volumes and the two
   lines.
4. During the copy: `docker network disconnect` for 30 s; `docker restart` of the old one; `docker kill` of the new
   one and a start again.
5. Wait for `move: finished` in the new server's log. Point the uploader at the new server.
6. Check: the same `fingerprint`; the same `token list`; for every log the uploader sent, `log export NAME` gives the
   same SHA-256 on the new server; `chain KEY` the same for each car; the old server answers `503 moved`; partial
   uploads carried on.
7. `move undo` on the old one: it takes uploads again.
8. `docker exec new /sentry-mate export > x.smexport` while uploads go on; copy it into a third container's empty
   data folder; `import` with the recovery key on standard input; the same checks.
9. A full disk: the new server's data on a 64 MB tmpfs volume: it says how much is missing and waits; `move cancel`
   removes the copy and tells the old server.
10. A second image with a lower version number as the new server: refused, and the code still works with the right
    image.

Afterwards the containers, volumes and test images are removed, and the build cache pruned.

## 10. Build order

1. **vault**: open a vault from a data key (writes a new key file and `vault.json` with a given recovery part, like
   `restore` does with the recovery key); give a vault's recovery part; seal a stream with a given file id from a
   given chunk on (for resuming an export); the end of a records file's last whole record, from its lengths only.
2. **ingest**: the write gate on every write (upload, first tie, sweep, clean-up, token commands) and `.lock`; the TLS
   exporter value in `Conn` (taken once after each handshake); upload paths answering `503 moving` and `503 moved`; a
   place for the move routes in the router; `cert.pem` written under a temporary name and renamed, like everything
   else.
3. **A new crate, `moving`**: the walk and the copy rules (one place for kinds, what isn't carried, walk order, the
   digest); the code; the old side (state in `.move/`, the `/api/move/v1` routes, pause and finish); the new side (the
   pull loop over hyper's client and tokio-rustls with the pin, `.moving/`, the switch); the export (snapshot, writer,
   ranges) and the import (reader, checks, `.importing/`, the same switch).
4. **sentry-mate**: start-up order (`.lock`, `.moving/`, `.importing/`, the env lines, the unused check, then the
   vault); the commands of section 5; `health` during a move; the request files.
5. **Docs**: README (Move to another server, Export and import), the TrueNAS guide (the two lines), the compose file
   comments.
6. **Tests** of section 9, with the Docker script.
7. **Wave 2**: the `/api/v1` endpoints of section 6 and the three UI screens.

Not in this plan: moving only some cars, or merging two servers into one; pushing from the old server to a new one that
can't be reached (use the export file); moving a TPM-sealed key file (when TPM sealing comes, a move writes a new one
on the new machine the same way).
