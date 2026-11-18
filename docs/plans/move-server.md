# Plan: moving to another server in one step

Status (2026-11-18): built, tested, and tried by hand with the binary: the export file, the import, the write gate
and the snapshot (crates `gate` and `moving`), and the direct move (`moving::code`, `proto`, `old`, `pull`,
`client`; the `move` commands, the two environment lines and `SENTRY_MATE_OFFER_MOVE` in `sentry-mate`). Not built:
the web server that answers the `/api/v1` of section 6 (wave 2); its pages and API are designed and served by the
dev server on made-up data. The Docker test of section 9 is `tools/move-e2e.sh`. Revised
after a design review, while building, and after a review of the built move: see **Review notes** at the end. The
owner's guide is `docs/moving.md`. The owner's request: "Make sure there's a one click export so if we needed to migrate to
another server there's no 15 step in ssh to get it moved."

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
   stopped taking uploads. The old server's log then says `move: done`: only then can it be stopped.
3. **If the new server has another address, change it in the CAN Bridge app** (Second Sentry Six Manager, server
   address). The token and the certificate SHA-256 stay as they are. Nothing is lost meanwhile: the bridge keeps its
   logs and sends them once it reaches the new server.

Afterwards the two lines can go (they are ignored once the move is done). Keep the old server's folders a while as a
backup, then delete them. The recovery key stays the same.

Tip: if the new server will end up with the old one's address (the address moves over), the bridge needs no change at
all. **Same machine, new disk or pool**: the old app already holds port 10100, so the new app can't use it during the
move (Docker and TrueNAS refuse: "port is already allocated"). Give the new app another host port for the move, for
example `10101:10100`, and point `SENTRY_MATE_MOVE_FROM` at `127.0.0.1` or the machine's address. After `move:
finished` and the old server's `move: done`, stop the old app and change the new app's port back to `10100:10100`.
The bridge then needs no change. The new server's log says this when it moved from an address of its own machine.

Later, in the web UI, the same steps are buttons: Settings → Server and access → **Move to another server** shows the
code; the new server's first-run screen has **Bring my data from another server** (address and code).

Export file: `docker exec <container> /sentry-mate export > sentry-mate.smexport` on the old server. On the new one,
put the file in its data folder and run `docker exec -it <container> /sentry-mate import
/data/sentry-mate.smexport` (it asks for the recovery key). Later in the UI: **Download an export**, and **Bring my
data from an export file** on the first-run screen.

## Decisions

- **Bytes as stored.** Nothing is opened, decoded or sealed again: every file is copied exactly as it is on disk, at
  the same path under the data folder. Each file's sealing is bound to that path, so it opens on the new server.
- **Three kinds of file**, told apart by their first bytes: sealed whole files, growing files (vault records), plain
  files. Each kind has one copy rule. New crates' files are carried without changes here.
- **One rule for what is carried**, in one function (`moving::tree`), used by the list, both digests, the export and
  the import. So an export Sentry Mate made always passes its own import's checks.
- **One write gate with a door.** Writers hold a shared lock while they write; a pause closes the door first, so new
  writers wait and the pause can't be starved. The move's last round and an export's snapshot each need one pause of
  seconds; nothing else stops.
- **The move code**: 125 random bits in 30 symbols, valid 24 hours, used once, home network only by default. It holds
  no address and no certificate (shorter to type). The claim proves the code over the TLS connection itself, so a fake
  server or someone in between learns nothing. Its check is a CRC: every typo of one symbol is found.
- **The data key travels last.** It leaves the old server only in the finish answer, wrapped with a key from the code,
  inside TLS 1.3. The recovery part (the data key wrapped by the recovery key, as in `vault.json` and an export's
  header) travels in the claim answer, so a half-done copy can always be opened with the recovery key. The key file
  never leaves the keys folder; the new server writes its own.
- **Finish is automatic**: once caught up, the old server pauses, the last changes are copied, the old server confirms
  and records that it moved, the new one switches and says so. Nothing to press. The bridge's logs wait on the bridge
  meanwhile. Automatic is the safer choice: the pause lasts seconds, and the commit point is one file on the old
  server, so no step is left for the owner to forget (a forgotten "finish" would leave two servers taking uploads).
- **The new server must be new**: nothing of the owner's in its data folder. Only a first start's empty vault may be
  there, and it is replaced.
- **Export**: a snapshot made with hard links in a pause of seconds, then read while the server runs. Its bytes are the
  same every time, so a cut download resumes. It opens with the recovery key only and never holds the key file.
- **Versions**: the new server must run the same version or a newer one. An older one is refused before the code is
  used up.
- **No new dependencies**: rustls with ring (already there), the vault's own primitives (`hmac`, `hkdf`,
  `chacha20poly1305`), rustix (already there) for the locks. The new server's client is a small blocking HTTP/1.1
  client over rustls (`moving::client`): hyper's client side would have pulled in new crates. No C code, and
  `Cargo.lock` gains no crate.

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
   by a new one (a fold, a rewrite in format 2, an upload started again), which has a new file id.
   - **While writes go on** (the move's rounds before the pause): copied up to the end of the last whole record, found
     from the record lengths alone (no key needed). The next time only what was added is fetched, after two checks on
     the old server: the file still starts with the same 22 bytes (same file id), and the 16 bytes before the copy's
     end are still the same. Those 16 bytes are the last copied record's tag, and every record's tag is bound to the
     one before it, so this one tag stands for the whole copy. If either check fails, the file is copied whole again.
   - **While nothing writes** (the move's paused round, an export's snapshot): the **whole file as stored**, tail
     included, with its time. A tail that isn't a whole record is accepted only then. This matters: the db's journals
     read what comes after the last whole record, and report anything that is neither a torn record nor zeros as
     damage instead of cutting it off (`db/src/journal.rs`). A copy cut at the first length that doesn't read would
     turn that reported damage into silent loss on the new server.
3. **Plain files** (anything else: `vault.json`, `cert.pem`, the markers, public downloads). Written once or renamed
   into place. Copied whole, again when the length or the time changes.

Nothing else is ever changed in place. That becomes a rule for every crate, wave 2 included: **a file is either
written whole under a temporary name and renamed, or it is a vault records file that only grows.** A crate that ever
needs something else must add a kind here first. A test walks the data folders the crates' own tests make and checks
every file is one of the three kinds.

A file's identity, for "has it changed?", is its length and its modification time (in microseconds). For sealed and
growing files it is also the file id (bytes 6 to 22), and for growing files the last tag, as above. Two more rules make
that exact:

- **Times are kept.** Each copy gets the old file's modification time, set only once the copy is whole and synced. The
  new server then compares its copy with the old file by length, time and file id. It also keeps `clean_stale` right
  (it drops unfinished uploads two days old, by their time).
- **Racy times.** A file whose time is less than 2 seconds before the old server read it (`0 <= read_at - mt < 2 s`, by
  the old server's clock) is fetched again in the next round. On a file system with coarse times, a second change with
  the same length could otherwise keep the same time and be missed. A time in the future (a clock that was fast) is
  never racy: any write sets the time to now, which differs from it, so a change is always seen. The paused round
  starts 2 seconds into the pause, so nothing is racy then.

Files removed on the old server are removed from the copy.

### What is carried: one rule

One function decides, used by the move's list, both digests, the export, the import's checks and the "is this server
new" check:

- A **regular file** is carried when every part of its path is UTF-8, made of letters, digits and `._-` only, not
  `.` or `..`, and the whole path is at most 1,024 bytes. Every name Sentry Mate makes fits this rule.
- **Never carried**:
  - `vault.json`. Its recovery part travels in the move's claim answer or the export's header; the new server writes
    its own with its own key file.
  - Anything at the top of the data folder whose name starts with a dot: `.lock`, `.gate`, `.gate-door`, `.move/`,
    `.moving/`, `.importing/`, `.export/` (all below), and later `.backups/` (the automatic backups before an upgrade
    belong to the server that made them).
  - Temporary files (the suffixes above), anywhere, and `db.restoring/`.
  - `*.smexport` at the top of the data folder (an export file put there to import).
  - In the export file only: the public downloads (`geonames/`, `maps/`). They can be large, anyone can download them
    again, and leaving them out keeps every file in an export snapshot one Sentry Mate wrote itself. The new server
    downloads them again from the same source once places is wired in. The direct move does carry them.
- **Skipped and named** in the log and in `move status` (both sides skip them the same way, so the digests still
  match): symbolic links and anything that isn't a regular file or a folder (Sentry Mate never makes them), folders
  the server can't read (`lost+found` at the root of an ext4 disk, owned by root), files it may not open (one copied
  in as root with mode 0600: the walk sees it, opening it is refused; the list, the digest and the export skip it
  alike), and names that fail the rule
  (Synology's `@eaDir/`, a `notes (1).txt` someone left). They are left where they are.

### Walk order

Both servers, the export and the import go through files in one order, **walk order**: paths compared part by part,
each part by its bytes, and a part that is the start of another comes first. So `tokens/ff….key` comes before
`tokens.json` (the part `tokens` is the start of `tokens.json`), and `a/x` before `a-b` and `a.b`. This is the order of
a depth-first walk with each folder's entries sorted by name bytes. It is **not** the byte order of whole paths (`.` is
0x2E, `/` is 0x2F): comparing whole paths as strings would put `tokens.json` first. The one comparator
(`moving::tree::walk_cmp`) is used for the list's `after=`, the one-pass compare of the list with the copy, both
digests and the import's order check.

### One consistent moment: the write gate

Files depend on each other: a trust head names the generation of its records, and the db's fleet counts the rows of
the cars' tables. Copied one by one while writes go on, they can come from different moments. That is fine while
catching up, because the new server uses nothing until the end. The last round needs one moment. So:

- Two empty files, `<data>/.gate` and `<data>/.gate-door`, and plain `flock` locks on them (crate `gate`).
- **A writer** closes the door behind it to go in: it takes `.gate-door` exclusively, takes `.gate` shared, and lets
  go of the door at once. It holds `.gate` while it writes: an upload chunk with its trust record and its token's
  first tie, the sweep, the clean-up, a token made from the command line, the start-up's own writes; later every db,
  accounts and places write. Long jobs (a rebuild) hold it per step, never for the whole job. An upload takes it only
  once the chunk has arrived, so a slow network never holds it.
- **A pause** takes `.gate-door` and keeps it, then takes `.gate` exclusively. It waits only for the writers already
  inside (at most 60 s, else the pause fails and is tried again later). New writers wait at the door. This matters:
  `flock` gives a waiting exclusive lock no priority over new shared ones, so without the door a busy server (300 cars
  sending chunks that overlap) would never let a pause in.
- In the server, the writers are tasks of one process: they count themselves in memory behind a lock that lets a
  waiting pause go first, and the process holds `.gate` shared while any of them is inside. Each new writer still
  looks at the door (one non-blocking `flock`), so a pause from another process (a command-line export) stops them too.
- **After it holds the gate, every writer checks again** whether the server is paused or has moved: the server in
  memory, a command in `.move/` under the lock. A writer that got in just after a move's commit is refused (`503
  moved`), never answered `2xx`. Built in the server as a gate that can be **closed**: the check and the count are
  one step under one lock, so no writer gets in after the gate closed, one that was waiting at that moment included.
  (The command's look at `.move/` comes with the move.)
- Upload requests wait at most 20 s for the gate, then answer `503 moving` with `Retry-After: 60`; the bridge tries
  again at its next pass.
- Pauses are short: the move's last round (seconds, about a minute at most) and an export's snapshot (about 20 µs a
  file: some 20 seconds for a million files).
- `<data>/.lock` is held exclusively by the running server its whole life. A second server can't open the same data
  folder (two containers on one volume): it says so once and waits, rather than exiting into Docker's restart loop.
  The command line can tell whether a server runs.
- An upload's writes run in a task of their own that holds the pass, so a request dropped part way (its 10 minutes
  ran out) never leaves a write running outside the gate.
- All are plain `flock` locks on a local file system (ZFS, ext4, xfs, btrfs). A data folder on a network share
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
- 4 symbols: a 20-bit check, so a typo is found before anything is sent. It is a CRC (x^20 + x^3 + 1) over the 26
  symbols before it, so every wrong symbol and every two symbols swapped side by side are found, always, not just
  nearly always.

It has no address and no certificate SHA-256 in it. The owner gives the address beside it (the UI asks for both), and
the certificate is learned in the claim, proven by the code itself (2.3). That keeps the code short enough to type.

From the secret, with HKDF-SHA256 (salt `sentry-mate move code v1`):

- the **auth key** (`auth`): proves the code in the claim, and makes the session;
- the **wrap key** (`wrap`): wraps the data key in the finish answer;
- the **code id** (`id`, 8 hex digits): names the code in logs and status lines. The code itself is never logged,
  except when the owner asked for it with `SENTRY_MATE_OFFER_MOVE`.

Rules:

- Made by `move code`, or the UI (admin, password and two-step code again), or by `SENTRY_MATE_OFFER_MOVE` on start.
- Valid **24 hours** to be claimed. Used **once**: the first claim takes it. **One at a time.**
- **A claimed move is ended only on purpose**: by `move cancel`, or by `move code --replace` typed by hand (it warns
  that a move is under way and asks to type `yes`). A code made by `SENTRY_MATE_OFFER_MOVE` never ends a claimed move,
  and neither does a restart of the old server.
- `move cancel` revokes a code not claimed yet.
- **Home network only** by default (loopback, private IPv4, link-local, IPv6 unique local: the rule of
  `accounts::net`). `move code --anywhere` (or `SENTRY_MATE_OFFER_MOVE=anywhere`) for a new server elsewhere, such as
  a rented server. This is a second fence; the code is the real one.
- Kept on the old server in `.move/code.sealed` (sealed with the vault): the auth key, the wrap key, the code id, when
  it expires. Never the code, except one made by `SENTRY_MATE_OFFER_MOVE`: that one is printed in the log again on
  every start until it is claimed (the log is the owner's only screen, and a restart would otherwise lose it). It is
  dropped from the file at the claim. (The auth key itself is needed: an HMAC can't be checked with only its key's
  SHA-256; and anyone who can read `code.sealed` holds the data key already.)
- **Rate limits**: 10 wrong tries from one address (IPv6: its /64) in 10 minutes, then `429 slow_down`. 100 wrong
  tries in all, and the code stops working (the log says so; make a new one).
- The code in the log (`SENTRY_MATE_OFFER_MOVE`): with no shell, the log is the only screen the owner has. That is
  acceptable because the code works once, for 24 hours, and gives the data key only at the end of a whole move, which
  stops the old server and says where the data went. The log line says not to share it.

### 2.2 The protocol on port 10100

The move uses the upload port: the same TLS 1.3 server, the same connection guards (6 connections per address, a
request at most 10 minutes). Its paths are under `/api/move/v1/` (`moving::old::router`, merged into the upload API's
router). With no code made, no move under way and no move done, every one of them answers `404 not_found` like any
unknown path, and so does a wrong method, so a scan learns nothing. Errors have the upload API's shape: `{ "error":
"<words>", "code": "<code>" }`, with more fields where the table says (`at_ms`, `from`, `app`, `expired_ms`,
`files`, `bytes`). The upload paths of a moved server, and of one switching to an import, answer `503` with the
gate's reason (`moved`, `moving`) and `Retry-After`: `ping` and `status` too, not only the chunks.

| Path | Method | Proof | Body and answer |
|---|---|---|---|
| `/api/move/v1/claim` | POST | the code, bound to the connection (2.3) | `{ v: 1, claim, app, proof }` -> `{ session, server_proof, app, cert_sha256, files, bytes, recovery }` |
| `/api/move/v1/list` | GET | session | `?after=<path>&limit=` (at most 20,000; `after` in walk order) -> lines of JSON, one per file, then `{ end: true, next, now_us, paused, skipped, skipped_count }` |
| `/api/move/v1/fetch` | POST | session | `{ want: [{ p, from, to, v, tail? }] }` (at most 4,096) -> frames of bytes, at most 64 MiB |
| `/api/move/v1/pause` | POST | session | -> `{ pause, until_ms }` once writes have stopped; `503 busy` (Retry-After) when they didn't stop within a minute; `{ pause: "moved" }` on a moved server |
| `/api/move/v1/finish` | POST | session | `{ pause, files, bytes, digest }` -> `{ moved_ms, key_id, data_key }` |
| `/api/move/v1/switched` | POST | session | `{ at_ms }` -> `{ ok }`: the new server switched and serves; the old server may now say "done" |
| `/api/move/v1/status` | GET | session | -> `{ state: "copying" \| "paused" \| "moved" \| "done", pause, moved_ms, switched_ms }` |
| `/api/move/v1/session` | DELETE | session | the new server gives up: the session ends, a pause ends (refused once moved) |

A list line: `{ "p": "can/abc123/abc123_20261108T143000Z_1.log.xz", "k": "s", "len": 233006, "mt": 1794148261123456,
"id": "<32 hex>" }`. `k` is `s` (sealed whole), `r` (records) or `p` (plain). Sealed and records lines have `id`, the
file id; a sealed or records file shorter than its 22-byte header counts as plain (it has no file id). In the rounds
before the pause, a records line's `len` is the end of its last whole record; in the paused round, and on a moved
server (whose lists say `paused: "moved"`), it is the file's whole length. The new server checks every line: a path
the walk carries, in walk order, after `after`. Paths are in walk order (section 1), so the new server compares the list with its
copy in one pass. About 120 bytes per file: a million files make about 120 MB per round, a second or two on a home
network.

A fetch want names the version it continues, `v`: the file id (hex) for sealed and records files, `len:mt` for plain
files, as the list gave them. A file opened on the old server that doesn't match is answered "changed" with no bytes.
So two versions of a file are never mixed, whether it comes in pieces, on several connections or after a restart.

A fetch answer is binary:

```text
frame = 0x01 | varint path length | path | u64 from | u64 count | u64 length now | u64 time (µs) | u64 read at (µs)
        | flags (u8) | the bytes
        flags: 1 changed (not the version wanted, or the tail didn't match: no bytes; fetched whole next round), 2 gone
end   = 0x00 | varint frames
```

Wants a 64 MiB answer can't hold are left out and asked for again, and the last one answered may come shorter than
asked. A want asks for at most 16 MiB of a file. So a request stays far inside the 10-minute limit even on a slow
link, and big files come in pieces that can be resumed.

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
  refused with `409 too_old` and the code stays usable. Then the code is taken. The **session** is
  `HMAC-SHA256(auth key, "sentry-mate move v1 session" | claim)`: 256 bits, and the old server keeps only its
  SHA-256 and the claim. The answer carries the session, the old server's version, its certificate's SHA-256, the
  files and bytes to copy, and the **recovery part** of its `vault.json` (the data key wrapped by the recovery key:
  salt, Argon2id settings, nonce, ciphertext, key id). `server_proof = HMAC-SHA256(auth key, "sentry-mate move v1
  server" | exporter | session | app | cert_sha256 | files | bytes | recovery)`, each field with its length in front.
  The recovery part is what every export's header carries anyway: it opens only with the recovery key, so this shows
  nothing new.
- The new server checks `server_proof` before it uses anything, then saves it all in `.moving/state.sealed`. From
  then on it pins the old certificate by that SHA-256 on every connection and sends `Authorization: Bearer <session>`.
- A fake server, or someone in between, sees one HMAC over a value of its own connection. With a 125-bit secret it
  can't be guessed offline. The secret and the keys never cross the network. The new server tells the owner: "the
  server at 192.168.1.20 couldn't prove it made this code: nothing was sent".
- A lost answer: the same code with the same `claim` gets the same session again (it is made from the claim). Any
  other claim gets `409 code_used` with when and from which address it was used.
- The session lasts until the move ends. While copying, 7 days with nothing from the new server end it (the old
  server says so). **Once moved, it is kept until undo**, with no time limit, and `finish`, `switched` and `status`
  keep answering for it: a new server that was off for weeks after a lost finish answer still gets it.
- A proxy that ends TLS in front of the upload port would break the bridge's pin anyway. Pass-through (TCP) proxies
  are fine.

### 2.4 Copy rounds

- **Round 1** lists everything and fetches everything. The new server checks the free space first: the total from the
  claim plus 2 GiB (ingest's `MIN_FREE`). With less, it waits and says how much is missing.
- **Later rounds** list again. The new server compares each line with its copy: missing, or a different length, time
  or file id: fetched whole (a growing file of the same id: only what was added); fetched racy last time: fetched
  again; on its side but not in the list: removed.
- Four connections fetch at once (the server allows six per address).
- **Each fetched file goes into `.moving/part/<n>` first.** Once it is whole it is given the old file's time, synced,
  and only then renamed into `.moving/data/<p>`. A growing file's catch-up is added to its copy in order, whole
  records only, synced, then timed (a stop before the time is set leaves the new length with the old time: the next
  round sees a different length and checks the tail, as for any growing file). So a file in `.moving/data/` is always
  one whole version of the old file, and its length, time and first 22 bytes say which.
- **No index of every file**: the copy is its own index. What isn't settled (racy files, pieces of big files fetched so
  far, with the version they continue) is small and listed in `.moving/state.sealed`, written after each batch. A
  restart neither trusts a partial file nor forgets a racy one. A round can stop anywhere (a cut network, a restart of
  either server): the next one fetches only what is still missing.
- The new server checks what it writes: a sealed file starts with `SMENC` and has a valid sealed length, and the
  bytes fetched for a growing file are whole records (except in the paused round, where the tail comes as stored).
- **When to pause**: after a round that found at most 64 MiB and 1,000 files to fetch. There is no forced pause after
  so many rounds: if the old server keeps changing faster than the copy catches up (a TeslaMate import running there,
  say), the new server waits a minute between rounds and says how much changes per round, so the owner can let that
  job end first. Uploads are never held for a pause that can't finish quickly.

About 180 GB and a million files take about half an hour on gigabit; the bridge's uploads (under a megabyte a minute
per car) make each catch-up round small.

### 2.5 Finish (automatic)

1. **Pause.** The new server asks for the pause. The old server closes the door and takes the gate exclusively
   (waiting for writes under way), waits 2 seconds (so file times settle), and answers with a pause id. Uploads wait,
   then get `503 moving`. When the pause starts, the old server walks its tree once (stat only: the heads it read in
   earlier rounds are kept in memory by inode, length and time) and keeps that walk's digest in memory under the
   pause id.
2. **Last round.** The new server lists and fetches what changed since the round before, whole files as stored, and
   compares file ids too. Nothing is writing now, so this round is exact. The lists are walked again (stat only, the
   heads come from memory): nothing writes, so they give the same lines as the pause's walk. (Built this way instead
   of a sealed `.move/pause.list`: same result, one file less.)
3. **Finish.** The new server sends the number of files, their bytes and a digest: SHA-256 over
   `path \t kind \t length \t time \t file id \n` for every file it holds, in walk order (the file id is empty for
   plain files).
4. **The old server checks** against the digest kept with the pause id. Different: `409 digest_mismatch` with its own
   count; it walks once more for a fresh digest (so a change made by something else during the pause is caught up
   with), the new server does another round, and the pause goes on. After five such rounds the new server lets the
   pause end and catches up first. The same: under `.move/lock` it **reads the code record again**, and goes on only
   if the move is still the one the request was let in for (the same code, claim and session, and the session not in
   `cancelled.sealed`); a `move cancel` or `move code --replace` run meanwhile wins, and the finish answers `410
   cancelled`. The pause must also still be the one asked about (`409 not_paused`). Then it
   **sets "moved" in memory first** (every writer that gets the gate from now on is refused), writes
   `.move/moved.sealed` (synced: this is the commit point), and **keeps the gate exclusive for as long as it is moved**
   (only undo lets go). Then it answers with:
   - `data_key`: the data key sealed with the wrap key (XChaCha20-Poly1305, a random nonce, associated data
     `sentry-mate move v1: data key` | key id | session);
   - `key_id` and `moved_ms`.
   The pause becomes the moved state (2.6). The check, the commit and taking the hold are one step on a blocking
   thread, under the pause's lock: a new server whose connection drops during the finish can't leave the old server
   moved on disk but not in memory, and the ticker sees all of it or none. (Should that ever happen all the same, the
   ticker takes the hold itself; it never takes a moved state for a cancel.) Its log says "moved: the new server is switching; keep this server running
   until it says done".
5. **The new server** opens the data key, checks its key id, and **writes the answer into `.moving/state.sealed`,
   synced, with the state `switching`**, before it touches anything else. Then it switches (3.3), starts serving, and
   sends `switched`.
6. **The old server**, on `switched`: writes it into `moved.sealed`, and only now logs "done: ... you can stop this
   server now".

- **Timers**: the pause ends by itself when the new server sends nothing for 60 s, or after 15 minutes in all. A
  finish, list or fetch in progress stops the 60 s timer until it is answered, so a slow walk on a big HDD folder can't
  end the pause under a finish. The 15 minutes hold whatever is being answered. A fetch answer is sent a megabyte at a
  time, and each piece must be taken within 60 s (`FETCH_STALL`): a new server that stops reading (its disk hangs,
  its container is paused) has the answer stopped, and its end doesn't count as hearing from it, so the pause ends 60
  s after the last piece it took. When the pause ends, the old server takes uploads again, and the new one catches up
  and asks again.
- A lost finish answer: the new server asks again and gets the same answer (rebuilt from `moved.sealed` and the
  session, a new nonce), also after a restart of the old server, and also weeks later.
- **The old server is gone before the finish** (its disk failed, say): the new server holds most of the data and the
  recovery part. `move finish --recovery-key` on the new server asks for the recovery key and switches to what it has.
  It warns first: the last changes may be missing, the copy may not be of one moment, and the old server doesn't know
  it moved, so it must not be started again with the bridge pointed at it.
- Why the key comes last: someone who got hold of the code gets nothing they can read unless they finish a whole move,
  and finishing stops the old server and makes it say where the data went.

### 2.6 The old server after the move

- `.move/moved.sealed` says when, to which address, which session and code id, and when the new server confirmed. It
  stays until undone. It is never carried (it is in a dot folder).
- It holds the gate exclusively the whole time, also after a restart (taken before anything else on start). Every
  upload path answers `503 moved` with `Retry-After: 3600` and "This server's data moved to another server: change
  the server address in the CAN Bridge app." The bridge shows "answered 503 moved; will retry" and keeps its logs.
- It writes nothing more: no housekeeping, and commands that write refuse ("this server's data moved to 192.168.1.30 on
  2026-11-17 at 14:02"). Reading still works (`log export`, `chain`, `fingerprint`, an export file: an export of a
  moved server needs no pause, nothing writes; later the UI, read only, with a banner).
- The move's session keeps answering `finish`, `switched` and `status` until undo.
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

- What a first start makes by itself doesn't count (`vault.json`, `tls/cert.pem`, `tls/key.sealed`, `migrated.json`,
  empty folders, the empty markers under `can/.waiting/`), and neither does an export file put at the top of the data
  folder to import, nor what the carried rule skips. So a server started once without the two lines can still take a
  move: its own empty vault is set aside and removed at the switch, with the `RECOVERY-KEY.txt` it made (that key
  would open nothing; the log says so).
- **The data folder's disk must keep exact file times.** A copy is matched to the old file by length and time (in
  microseconds). A disk that rounds them (whole seconds: ext3, ext4 made with 128-byte inodes, FAT, exFAT, some
  network shares) would make every copied file look changed, so the move could never finish. Before the claim (the
  code is not used up) the new server sets a time with microseconds on a test file and reads it back. If it comes
  back rounded, the move is refused with `disk_times`: "put the data folder on a disk that keeps exact times (ZFS, as
  TrueNAS uses, or ext4, xfs, btrfs)". Not taken: comparing times at the disk's precision, which would need the old
  times kept beside every copied file for the digest.
- The keys folder must be empty, or hold only that first start's key file. A key file that doesn't open the vault in
  this data folder is refused ("give the new server an empty keys folder"): it may belong to data elsewhere.
- A data folder in use is never touched. With the two lines set, the server starts as usual and warns: "nothing was
  moved: this server already has data; remove SENTRY_MATE_MOVE_FROM and SENTRY_MATE_MOVE_CODE".

### 3.2 During the move

- Everything copied goes to `.moving/data/`, at the same paths, times kept. A half-moved folder never looks like a data
  folder.
- Before the claim the new server makes **a key of its own for the move**, `<keys>/.moving/state.key`. Its move
  state, `.moving/state.sealed`, is sealed with it (XChaCha20-Poly1305, a key from it by HKDF): the address, the old
  certificate's SHA-256, the session, the claim bytes (saved before they are sent), the keys from the code, the
  recovery part, the round, the progress, what isn't settled (2.4), the last problem, and once finishing, the finish
  answer. A restart reads it and goes on. The vault itself (`.moving/vault.json` and `<keys>/.moving/sentry-mate.key`)
  is written at the switch, from the finish answer, or by `move finish --recovery-key`: before the finish the new
  server doesn't hold the data key, so there is nothing to wrap yet. The old server's key file never leaves its keys
  folder.
- `.moving/lock` is held shared while the copy writes and exclusively by `move cancel` and `move finish
  --recovery-key`, so a cancel never removes the folder under a write, and a write never makes the folder again
  after a cancel.
- **The copy can always be opened with the recovery key**, from the first byte: even if the old server dies at 90 %,
  `move finish --recovery-key` opens what was copied.
- The upload port stays closed until the switch. `sentry-mate health` answers healthy while a move runs, so Docker and
  TrueNAS don't mark the app as failing.

### 3.3 The switch

The same for a move (`.moving/`) and an import (`.importing/`). Every step can run again after a crash: on start, the
staging folder's state is looked at before anything else, before the vault is opened or made. (Built for the import:
`moving::import::switch`.)

0. The staging folder is made under another name (`.importing.new/`, and `<keys>/.importing.new/` for its key file)
   with its vault and its first state, then renamed into place: so a staging folder always has a state that opens.
1. Just before the switch, the data folder is checked again: still nothing of the owner's in it (3.1). Then the state
   says `switching` (written and synced before step 2; for a move it also holds the finish answer).
2. Each entry at the top of `<staging>/data/` is renamed into the data folder (one file system: instant at any size).
   If the data folder has an entry of that name (a first start's `tls/`, `can/`, `migrated.json`), it is first set
   aside in `<staging>/replaced/`. An entry still in `<staging>/data/` hasn't been brought in, so a later run never
   sets aside what an earlier one brought in, and no list of what was done needs saving.
3. `<staging>/vault.json` is renamed to `vault.json` the same way (a first start's set aside): the data key wrapped by
   the new key file, and the old server's recovery part as it was. **The old recovery key opens the data here too.**
   (A move writes this file at step 1, from the finish answer.)
4. `<keys>/<staging>/sentry-mate.key` is renamed to `<keys>/sentry-mate.key` (a first start's set aside as
   `<keys>/<staging>/replaced.key`).
5. `.move/imported.sealed` (or `.move/moved-in.sealed`) is written: from where, when, the export id or code id.
6. If a first start's vault was set aside, its `RECOVERY-KEY.txt` is removed (that key opens nothing now; the log
   says so).
7. The staging folder is renamed to `.importing.done/` and removed, then `<keys>/<staging>/`. A start removes what a
   crash left of these.
8. The server starts as usual: the vault, the TLS key, the tokens, then the upload port.

The state is read with whichever of the vault files opens it (the staging one before step 3, the data folder's
after), so every step can be taken again.

A start that finds a switch begun (`switching`) finishes it before anything else. If it can't (a full disk, a file in
the way), the server **doesn't start**: it says why and tries again every 30 seconds. It never runs on a data folder
switched half way, with files of the import beside the vault of the first start. (Built.)

### 3.4 Environment variables

| Variable | Server | What it does |
|---|---|---|
| `SENTRY_MATE_MOVE_FROM` | new | The old server: `192.168.1.20`, `nas.local:10100`, or the bridge's whole upload address (`https://192.168.1.20:10100/api/can/logs`). Port 10100 unless given. |
| `SENTRY_MATE_MOVE_CODE` | new | The move code. |
| `SENTRY_MATE_OFFER_MOVE` | old | `yes`: keep a move code ready (home network only) and print it in the log. A new one only when there is none, or the one there expired unclaimed; never while a move is under way or after one. `anywhere`: the same, for a new server anywhere. Ignored once the server has moved. |

On start:

- `.moving/` is there: carry on with that move (the two lines may be gone by now; what is needed is in the sealed
  state).
- The data folder is unused and both lines are set: start the move.
- This server already took a move with that code: one line, "the move from 192.168.1.20 is done; you can remove the
  two lines".
- The data folder is in use: start as usual and warn (3.1).

The new server never exits over a move problem, because Docker would restart it in a loop. It says why in the log and
tries again where that can help (an unreachable server, a full disk), with the upload port closed and `health`
answering healthy. Where it can't (a wrong, used or expired code, a newer old server), it says what to do and then
starts as usual meanwhile: a new server with no data and no tokens, so no bridge can upload to it. With a new code in
the lines and a restart, it moves. It tries a failed code only once per start, so a wrong code isn't tried again and
again. A move started by `move from` while the server runs is seen within a second: the server stops its upload
port and runs the move.

**Cancelled on the old server** (`410 cancelled`): the new server keeps `.moving/data/` and says so. It is removed only
by `move cancel` on the new server. A later code, put in the lines, whose claim proves the same old certificate
SHA-256, carries on with that copy and fetches only what changed.

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

When the old server was on this machine (same machine, new pool):

```text
move: the old server ran on this machine. Stop the old app now, then give this app port 10100 again
      (10100:10100) and redeploy: the CAN Bridge then needs no change.
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
move: the old server changes faster than this copy catches up (2.1 GB in the last round); waiting a minute. If a big
      job runs there (an import), let it end first.
move: the move was cancelled on the old server. What was copied here is kept: with a new move code from the same
      server it carries on from there. To remove it: `move cancel` here.
move: 3 entries were skipped on the old server (not carried): lost+found (can't be read), @eaDir (name), ...
```

The old server:

```text
move: a move code was made (3f9a1c07), valid until 2026-11-18 14:05, works once, from this home network only
move: a server at 192.168.1.30 took the move code (Sentry Mate 0.4.2) and is copying the data
move: 192.168.1.30 has copied 50 % (91.2 of 182.4 GB)
move: uploads paused for the last changes (192.168.1.30 is catching up)
move: moved: the data moved to 192.168.1.30 at 14:02; it is switching now. Keep this server running until this log
      says done.
move: done: 192.168.1.30 serves the data since 14:02. This server no longer takes uploads and tells the CAN Bridge to
      try later. You can stop it now; keep its folders as a backup for a while, then delete them. To take uploads
      here again: `move undo`.
```

## 4. The export file

### 4.1 Format

```text
export = "SMEX" 0x01 | u32 header length (big endian, at most 64 KiB) | header (JSON) | body (sealed)

header = { "format": 1, "app": "0.4.1", "id": "<32 hex>", "made_ms": 1794924000000, "key_id": "<16 hex>",
           "recovery": { "salt", "kdf": { "m", "t", "p" }, "nonce", "ct" }, "files": 1234567, "bytes": 195833364480,
           "size": 196020000000 }

body   = the plaintext in sealed chunks (vault::chunks): chunk = nonce (24 bytes) | ciphertext of 64 KiB | tag
         (16 bytes), the last chunk shorter; keys from the data key and the export id (HKDF-SHA256); each chunk's
         associated data is its number, whether it is the last, and the label "sentry-mate export v1 " + hex SHA-256
         of everything before the body; the nonce is HMAC-SHA256 of the associated data and the chunk's plaintext

plain  = "SMXB" | u16 format
         | varint count | count x entry                    the manifest
         | count x ( entry | the file's bytes )            every file, its entry again just before its bytes
         | "SMXE" | varint count
entry  = varint path length | path | kind (u8: 1 sealed, 2 records, 3 plain) | u64 length | u64 time (µs)
```

- **It opens with the recovery key.** The header carries the recovery part of `vault.json` (salt, Argon2id settings,
  the wrapped data key). The key file is never in it, and neither is the data key, so the file alone is useless to
  someone who takes it: they would need the recovery key (256 random bits through Argon2id at 1 GiB).
- **All of it is authenticated.** The body is sealed with a key from the data key, chunk by chunk, so a changed, cut,
  reordered or swapped chunk doesn't open, and neither does a last chunk that isn't flagged as the last. The label
  binds the header (a header from another export, or an edited one, doesn't fit the body). The manifest is inside the
  sealing. File names and sizes are hidden too.
- **`size` is the whole file's length**, so a file cut short is found before anything is read, and the download has a
  size from the start.
- **Sealed twice over.** The files inside are still sealed as they were stored; the body seals them again. That costs
  one more pass of XChaCha20-Poly1305 and one of HMAC-SHA256, quick next to reading the disk, and hides the names.
- **Why the nonce comes from the chunk.** A resumed download must get the same bytes again, so a chunk is sealed the
  same way every time. With the chunk's number as its nonce (the vault's sealed-file format), a file that did change
  under a snapshot would be sealed twice with one key and nonce and give away the XOR of the two. That can happen:
  a growing file is linked into the snapshot, and a writer whose append failed cuts what that write left and writes
  over it. With the nonce made from the chunk's own bytes, the same chunk gives the same bytes, and different bytes a
  different nonce.
- **The manifest comes first**, so an import checks every path, kind and the space needed before it writes anything.
  **Each file's entry comes again just before its bytes**, so the importer splits the bytes into files holding one
  entry at a time, never the whole list (5 million files make a manifest of some 450 MB). It checks the two copies
  agree with two running hashes (`h = SHA-256(h | entry)`), one over the manifest and one over the entries before the
  bytes; at the end they must be equal. A running hash, not one hash over everything, so it can be saved and an
  import can carry on after a restart.
- An export opens with the recovery key that was in use when it was made. After `recovery-key new`, older exports
  still need the old key; the CLI and the UI say so.
- File name: `sentry-mate-<date>.smexport`.

### 4.2 Written while the server runs: a snapshot

1. A pause (door and gate, section 1).
2. Every carried file is looked at once (its first bytes give its kind). Sealed whole files are **hard-linked** into
   `.export/<id>/files/`, at the same paths. That is instant and takes no space. It is a true snapshot because the
   server never changes those files in place: it replaces them, and a link keeps the version it points to. **Growing
   files whose records are whole** (the record lengths, walked from the start, end exactly at the file's end) are
   linked too: only bytes after that length ever change, and the export reads only up to it.
3. **Plain files, and growing files with a tail that isn't a whole record**, are copied there instead, as stored,
   tail included, with their times: a plain file could be written over by something else, and a torn tail is cut off
   and written over by the next append. They are small: the certificate, the markers, and the rare journal a crash
   left torn (the public downloads aren't in an export).
4. The plan is sealed into `.export/<id>/plan.sealed` and `.export/<id>/entries.sealed`: the export id, the header,
   each file's path, kind, length and time.
5. The pause ends. The export is read from the snapshot while the server takes uploads as usual.

- Hard links need the data folder on one Linux file system: ZFS (TrueNAS), ext4, xfs and btrfs are all fine. Where
  links can't be made, the export is refused with that reason; the direct move still works.
- Each snapshot holds a lock while it is in use. A new export removes snapshots no one holds; the server removes one a
  crash left at its next start. Later, for the UI: removed 24 hours after its last download request, after 7 days at
  most, or by `DELETE`. While it is there, files deleted from the data folder keep their space (logs are rarely
  deleted).
- Memory: the walk holds one folder's names at a time (it sorts them), about 80 bytes a file, so a car with 400,000
  logs takes some 32 MB while its folder is looked at. Everything else is one chunk and one entry at a time, whatever
  the number of files or their size.

### 4.3 Downloading and resuming

- The snapshot can't change, so the export's bytes are the same every time it is read. Its length is known from the
  plan before the first byte, so the download has a size and the browser shows time left.
- A cut download resumes with a `Range` request (the `ETag` is the export id). The server finds the chunk holding the
  first byte asked for, rebuilds that chunk's plaintext from the plan and the snapshot, and seals on from there with
  the same key and the same chunk numbers. Same key and nonce only ever seal the same bytes: the nonce is made from
  them. (Built: `moving::export::Snapshot::reader`, from any byte.)
- The command-line export writes to standard output in one go (no resume needed for a local file). It refuses to write
  to a terminal.

### 4.4 Import

Into a new, unused server (3.1) only. Nothing in the data folder is touched until everything has been checked:

1. **Header**: the magic, a known format, an app version no newer than this one (else: "made by Sentry Mate 0.5.0;
   update this server first"), Argon2id settings within bounds (a made-up header can't ask for 100 GB of memory), and
   the file's length equal to `size` (else: "cut short").
2. **The target is unused** (3.1). Else refused. The command checks this before it asks for the recovery key (it
   would be typed for nothing), and the import checks it again before it writes anything.
3. **Recovery key**: Argon2id, then the data key, then its key id must match the header's; the first and last chunks
   must open with it. Wrong: refused, nothing written.
4. **A vault for the import**: a new key file in `<keys>/.importing/` and `.importing/vault.json` (the data key wrapped
   by it, and the export's recovery part). Its state, `.importing/state.sealed`, is sealed with it: which export
   (its id and the SHA-256 of its header), where the import has got to, and the running hashes.
5. **The body** is opened as it is read; every chunk is checked.
6. **The manifest** first, checked with the one rule of section 1: every path carried (letters, digits and `._-`, no
   `.` or `..` parts, at most 1,024 bytes, no dot entry at the top, no temporary name, no `vault.json`), in walk order
   with no path twice and no file inside another file's path; every kind known. Then the space: what is left to write
   plus 2 GiB must be free, or it stops before writing a file.
7. **Files** go into `.importing/data/` (times kept). Each must start the way its kind does. Every 256 MiB the files
   are synced (one `syncfs`) and the place reached is saved in the state: the next chunk of the body to read, the
   start of an entry the last chunk cut in two, the file being written and how much of it.
8. **The end**: the end marker, the count, the two running hashes equal, nothing after it, and the flag on the last
   chunk. A file cut short fails here.
9. **The switch**, as in 3.3. The state says `ready`, then `switching`.

- **After a restart the import carries on.** Run `import` again with the same file: it reads the state (no recovery
  key needed: the import's own key file opens it), goes to the saved chunk, cuts the file it was writing back to what
  was saved, and goes on from there. Another file discards a half-done import that is still being read; one that is
  `ready` is kept until `import --cancel` (nothing in the data folder was touched either way). A half-done import is
  never mistaken for a complete one: it is in a dot folder, and only `ready` or `switching` lead to a switch.
- **A fault of the file itself** (damaged, cut, a bad entry) or too little space removes the half-done import at once:
  running it again with that file can't help. A full disk or a stop part way keeps it, to carry on.
- **With a server running** (the usual `docker exec`), the command does all of the above but the switch, then marks
  the import ready. The server looks for that every second, stops its upload port, waits for its writes, switches,
  and starts again in place with the imported data (the container keeps running). The command waits for it and says
  when it is done. With no server running (`docker compose run --rm`), the command switches itself.
- From the UI the recovery key and the file's first bytes (the header) are sent first, so a wrong key is found before a
  large upload. The chunks are then opened as they arrive, so the upload is never stored twice. The state is saved at
  a sealed-chunk boundary, and after a restart `409 offset_mismatch` gives the offset of that boundary in the file, so
  the browser sends from there.

## 5. Commands

Run with `docker exec <container> /sentry-mate ...` (or `docker compose exec sentry-mate /sentry-mate ...`):

| Command | Server | What it does |
|---|---|---|
| `move code [--anywhere] [--replace]` | old | Makes the move code and shows it once, with when it expires. With a move under way it refuses, unless `--replace` (it asks to type `yes`: the move under way ends). |
| `move status` | either | What the move is doing: the code waiting, who is copying, how far, paused, moved (and when, to where), confirmed; what was skipped. |
| `move cancel` | either | Old: the code stops working, a move not finished ends, a pause ends; after the move: "finished; use `move undo`". New, before `switching`: the move stops and what was copied is removed (the old server is told). New, from `switching` on: refused, "the move is done; this server holds the data". |
| `move undo` | old | After a move: take uploads here again. Asks to type `yes` and warns about the new server. |
| `move from ADDRESS CODE` | new | Brings the data here. With a server running, the command proves the code (so a typo or a wrong code shows at once), the server does the copy, and the command shows the progress; closing it doesn't stop the move. With no server running (`docker compose run --rm`), it does the whole move and exits; the server tells the old one it serves when it next starts. |
| `move finish --recovery-key` | new | Last resort when the old server is gone: switches to what was copied, opened with the recovery key. Warns that the last changes may be missing. |
| `export` | any | The whole server as one export file, to standard output (it refuses a terminal). With `docker compose exec`, add `-T` so the bytes aren't changed by a terminal: `docker compose exec -T sentry-mate /sentry-mate export > sentry-mate.smexport`. It takes the gate's pause itself, so it works while the server runs. (Built.) |
| `import FILE [--cancel]` | new | Brings an export file in (put it in the data folder first, e.g. `/data/sentry-mate-2026-11-17.smexport`). Asks for the recovery key, like `restore` (so `docker exec -it`), or reads it from standard input. Run again after a restart to carry on. With a server running it waits for the server to switch (up to 3 minutes), else it switches itself. `--cancel` removes a half-done import (refused once switching). (Built.) |

`move code` prints:

```text
Move code (shown once; valid until 2026-11-18 14:05; works once; from this home network only):

  SMMV-1K3QX-7WD2M-ZP4HT-9BN6R-XC8VA-5JE0F

On the new server (empty data and keys folders), set:
  SENTRY_MATE_MOVE_FROM=<this server's address: the one the CAN Bridge uses>
  SENTRY_MATE_MOVE_CODE=<the code above>
or run there: sentry-mate move from <this server's address> <the code above>
```

How the commands reach the running server: through files, as `token revoke` already does. On the old server the
commands change `.move/` themselves, under `.move/lock`, and the server reads those files on every request of the
move and looks at them every few seconds (a cancel ends a pause, an undo opens the gate again). On the new server,
`move from` stages `.moving/` and `move finish --recovery-key` leaves a mark there; the server looks every second.
`export` takes the write gate itself for the snapshot, so it needs nothing from the server; on a moved server it
needs no pause at all. Commands that write hold the gate like the server's writers and refuse on a moved server and
on one a move is coming into; reading commands (`token list`, `fingerprint`) on a moved server read without the
gate (the server holds it for good).

## 6. API for the UI (`/api/v1`)

The API is in `docs/ui-api.md`, "Moving and first-run setup" (the resources) and "Decisions" (the rules), and the
dev server (`ui/devserver`, `mockapi/resources/moving.py`) answers all of it on made-up data. It replaced the
draft that was here. What changed from the draft, on the owner's request "no docker commands for moving: just go to
settings, then export":

- The old server's calls are under `/api/v1/system/` (`/system/move`, `/system/move/undo`, `/system/exports`), beside
  `/system/storage` and `/system/health`. A move code is `POST /system/move`; cancelling is `DELETE /system/move`.
- The first-run calls take a **claim** instead of the set-up code in every body: `POST /setup/claim { setup_code }`
  sets a cookie, and `/setup/fresh`, `/setup/move-here`, `/setup/restore` and `/setup/job` need it. A fresh server
  has a third choice beside the two of the draft: **Start fresh** (the first admin and the recovery key, shown once).
- Pages poll (`GET /setup/job`, `GET /system/move`, `GET /system/exports/{export}`) instead of the draft's event
  streams: the set-up pages have no session, and the answers are small.
- A restore sends `header` and `recovery_key` first, as the draft said, then chunks with `Upload-Offset`; a wrong
  offset is `409 wrong_offset` (the import's name, not `offset_mismatch`).
- Re-auth is the account's `password` (and `two_step` when it is on) in the body of `POST /system/move`,
  `POST /system/move/undo` and `POST /system/exports`; the export's file is then a plain link.

### From the web UI

What the web server (wave 2) calls for each, all built in `moving` (the web server wraps them, in a blocking task,
and maps `moving::Error` and `pull::Problem` to the error codes of `docs/ui-api.md`):

| Endpoint | `moving` and the server |
|---|---|
| `GET /setup` | No account yet (the accounts store is empty) is `needs_setup`. `state`: `moving_here` while `pull::pending` is `Moving`, `restoring` while `import::pending` is not `None`, else `fresh`; `set_up` once there is an account. `job` from `GET /setup/job`. |
| `POST /setup/claim` | New, not in `moving`: the set-up code, 60 random bits, made on the first start with no account, kept sealed in the data folder and printed in the log on every start until set-up is done; compared with `proto::same` (constant time); the sign-in throttle. The claim is a random token, kept in memory (a restart asks for the code again). |
| `POST /setup/fresh` | The accounts crate makes the first admin. The recovery key is the one the vault made on the first start (`RECOVERY-KEY.txt`); the answer reads it from there. `POST /system/recovery-key/saved` deletes that file; until then `POST /system/recovery-key/show` (password again) reads the key from it once more. |
| `POST /setup/move-here` | `pull::begin(data, keys, from, code, opts)`; its `Problem` gives the answer before anything runs (`address` -> `400` on `from`, `wrong_code` with a typo -> `400` on `code`, `unreachable` -> `502 old_server_unreachable`, the old server's refusals -> `move_code_wrong`, `move_code_expired`, `move_code_used`, `move_too_old`, `move_not_home_network`, `move_not_offered`, `not_empty` -> `409`). Then the server's own loop runs `pull::run`, as for `move from` and `SENTRY_MATE_MOVE_CODE`. |
| `POST /setup/move-here/finish` | `pull::force_finish(data, keys, recovery_key)`, then the switch on the next step of the loop. |
| `GET /setup/job` (move here) | `pull::status`: `phase` (`claiming` -> `connecting`; `copying` with round 1 -> `copying`, later rounds -> `catching_up`, the pause and finish -> `finishing`; `switching`), `progress` (`pull::Progress`; new: `cars_done` and `cars_total`, from the car folders whose files are all over, for the page's "N of M cars"), `problem` (`pull::Problem`: `retry` true -> state `waiting`, false -> `stopped`), `skipped`; `pull::moved_in` and `pull::Done` for `result` (`same_machine` from `client::Address::is_this_machine`). |
| `POST /setup/restore` | `import::unused` (the server is new), then the header: what `import::open_file` checks, read from the first 64 KiB (new: `open_file` reads a file today, the web server needs the same checks over bytes; `NotAnExport`, `NewerFormat`, `NewerApp` -> `not_an_export`, `export_too_new`), then the recovery key against the header (`WrongRecoveryKey`), then `import::begin` with the chunks fed as they arrive. A half-done import of the same export (`import::resume`) answers `200` with `received`. |
| `PATCH /setup/restore` | Feeds the chunk to the running `Import`; `received` is the last sealed-chunk boundary saved in the import's state (`Import::progress`). `Damaged`, `CutShort`, `Malformed` -> `failed` with `export_damaged` / `export_cut_short`; `NoSpace` -> problem `no_space`. After the last byte `Import::run` ends `Ready`, then `import::switch` (state `switching`, `done`). |
| `GET /setup/job` (restore) | `import::pending` and the import's `Progress` (`read`, `size`, `files_done`, `files`). |
| `DELETE /setup/job` | `pull::cancel` (tells the old server) or `import::cancel`; both refuse from `switching` on (`409 finished`). |
| `GET /system/move` | `old::status` (the code waiting or claimed, who copies, `sent_bytes` of `bytes`, the last cancel) and `old::moved_info` (`moved`, `switched_ms`). `state`: no code -> `none`; a code not claimed -> `offered` (or `expired` past `expires_ms`); claimed -> `copying`, `paused` while the gate is closed for the last round; `moved` from the moved record until `switched_ms`, then `done`. |
| `POST /system/move` | `old::make_code(data, vault, anywhere, replace)`; `Error::MoveUnderWay` -> `409 move_under_way`, a moved server -> `409 moved`. The code text goes in the answer only (`Made::code`). |
| `DELETE /system/move` | `old::cancel`: `Cancelled::Revoked` / `Ended` / `Nothing` -> `cancelled`; `Error::Finished` -> `409 finished`. |
| `POST /system/move/undo` | `old::undo`; `None` -> `409 not_moved`. |
| `POST /system/exports` | `export::Snapshot::make(data, vault, gate, app, wait)` in a blocking task (state `preparing` until it returns); `remove_unused_snapshots` first, so there is one at a time. |
| `GET /system/exports/{export}/file` | `Snapshot::size` for `Content-Length`, `Snapshot::reader(vault, from)` for the bytes from any `Range` start, the export id as the `ETag`, `Snapshot::file_name` for the name. |
| `DELETE /system/exports/{export}` | Drops the snapshot (its lock), then `remove_unused_snapshots`. |

Also: `/notices` adds `move_code_waiting`, `moving` and `moved` from `old::status`; a moved server answers `409
moved` to changes, as `old::refuse_if_moved` does for the commands; the audit log records a code made or
cancelled, a claim, the finish, an undo, an export made and downloaded, a restore and a move received.

What the pages show: the old server's page shows the code large with a copy button and its expiry, then "Waiting for
the new server", then a progress bar, then "Moved to 192.168.1.30: it is switching, keep this server on", then "Done:
you can turn this server off" with **Undo the move**. The first-run screen asks for the set-up code, then offers
the three choices; a move or restore shows its progress, then "Done: sign in with your account from the old server",
and the bridge note (with the port note when both ran on one machine).

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
| Old server restarted | The session is kept (sealed). A pause is gone: the new server catches up and asks again. The moved state is kept, gate and session with it | "the old server restarted; carrying on" |
| New server restarted | Carries on from `.moving/`: whole files already there are not fetched again, parts go on with the version they continue | "carrying on the move from 192.168.1.20 (45 % was copied)" |
| Old server's disk dies mid-move | The copy so far opens with the recovery key | `move finish --recovery-key`, with its warning |
| Finish answer lost | Asked again, any time until undo, also after either restarts | Nothing; it carries on |
| `OFFER_MOVE` with a move under way | No new code is made | Nothing; the move carries on |
| Disk full on the new server | Checked before round 1 (total plus 2 GiB); a full disk later pauses the copy, checked every minute | How much is needed and free, and how to give up |
| Disk full on the old server | Its own uploads fail as before (`507 no_space`). If it can't write the moved state, the finish fails, the pause ends by itself, the new server tries again | The old server's log says the disk is full |
| The new server goes away mid-move | The old server keeps serving; a pause ends after 60 s; the session is dropped after 7 days | `move status` on the old: "a move to 192.168.1.30 started ..., not heard from since ..." |
| Cancel on the old server while the new one finishes | One lock: whichever comes first wins. The commit reads the code record again under it, so a cancel or a new code made after the finish was let in still wins. Cancel first: the new server gets `410` and keeps its copy. Finish first: cancel says "finished; use `move undo`" | One of those two lines |
| The new server stops reading a fetch in the pause | The answer is stopped after 60 s without a piece taken; the pause ends 60 s after the last piece; at most 15 minutes in all | "the pause ended (the new server went quiet); uploads go on" |
| The new server's connection drops during the finish | The commit still finishes and is held; the new server asks again and gets the same answer | Nothing; it carries on |
| The new server's disk rounds file times | Refused before the claim (the code still works) | "keeps file times only roughly ... ZFS, ext4, xfs, btrfs" |
| `move cancel` on the new server after the switch | Refused | "the move is done; this server holds the data" |
| The old server keeps changing fast | No pause; rounds a minute apart | "changes faster than this copy catches up" |
| Both servers taking uploads | Can't happen through the move: the old one stops at the pause, the new one starts only after the old one wrote its moved state. Only `move undo` with the new one still running does it | The undo warning |
| A second server on the same data folder | `.lock` is held: it waits | "another Sentry Mate uses this data folder ...: waiting until it stops" |
| A file changed in place by something outside Sentry Mate | The digest of the last round catches a different length, time or file id | `digest_mismatch`, then another round |
| A folder or a file that can't be read, a name off the rule | Skipped on both sides (the lists, the digest, the export), named. A file this server's user may not open (made by root, mode 0600) is one of them | The skipped line |
| Same machine, port taken | The new app uses another host port for the move | The port note in the log |
| Export download cut | Resumes with `Range` while the snapshot is there; after that, `404 export_gone` | The browser resumes; or "make a new export" |
| Import: wrong recovery key | Refused before anything is written | "that recovery key doesn't open this file" |
| Import: file damaged or cut | Cut: found from `size` before anything is read. Damaged: stops at the first bad chunk and removes `.importing/` (the data folder was never touched) | "the export file is damaged or was changed at byte N: copy it again, or make the export again" |
| Import: restart mid-way | Carries on from the last saved chunk when `import` runs again; the server's log says one is half done | "carrying on the import of this file (45 % read)" |
| Import: the server is restarted during the switch | The start finishes the switch before it opens the vault; if it can't, the server doesn't start and tries again every 30 s | "import: switching to the imported data", then "import: done" (or why not, and "trying again in 30 s") |
| Import: not enough space | Found from the manifest before a file is written | How much is needed and free |
| Import or move into a server in use | Refused; nothing touched | "this server already has data" |

## 9. Tests

Unit (in the new crates, and in `vault` and `ingest` for their parts):

- Code: made, written and read back; every single-symbol typo found by the check; `O`/`I`/`L` mix-ups read right; wrong
  length or format refused; the code id doesn't give the code away.
- Claim: the right code on one connection is taken; a proof passed from one connection to another is refused; a wrong
  code counts, 10 from one address give `429`, 100 in all end the code; an expired, used or cancelled code is refused;
  the same `claim` gets the same session, another gets `code_used`; an older new server is refused and the code still
  works; the server proof (with the recovery part) is checked before anything is used.
- Copy rules: a sealed file copied whole and again only when its length, time or file id changes; a racy one fetched
  again, a future time never racy; a growing file copied to its last whole record (a torn last record left out) while
  writes go on, and as stored in the paused round (a damaged length in the middle kept, with what follows); then only
  what was added; a growing file whose failed write was cut off and written again is copied whole again; one replaced
  (new file id) copied whole; a piece fetched for another version answered "changed"; a restart with a part file half
  fetched never trusts it; a removed file removed; temporary files, dot entries, links and `vault.json` never carried;
  an unreadable folder, a file that can't be opened and a name off the rule skipped and named on both sides (the
  list, the digest and the export); times kept; a disk that rounds file times refused before the claim.
- Old side, after the review: a cancel or a new code made while a finish is answered wins over its commit; a finish
  whose pause ended is refused; a commit written but not held in memory is held by the ticker, and `move undo` still
  reaches the running server; the 15 minutes end a pause whatever is busy, and a stalled fetch's end isn't "heard
  from"; a fetch the new server stops reading lets the pause end (`tests/direct_move.rs`).
- Walk order: `tokens/` beside `tokens.json`, and `a/`, `a.b`, `a-b`: the comparator, `after=`, the merge and both
  digests agree.
- Consistency: trust folds, uploads finishing and tokens made during the rounds; after the last round under the pause,
  the copy opens and gives the same token list, chain reports and log counts as the old server.
- The gate: a pause waits for a write under way, holds new ones, gives up after 60 s; a pause is not starved by
  writers that overlap without end (in one process and across processes); uploads wait 20 s then `503 moving`; a
  writer blocked during finish gets `503 moved`, never `2xx`; the pause ends after 60 s of silence and after 15
  minutes, but not while a finish or list is being answered; `.lock` stops a second server. (Built: the gate's own
  tests.)
- Finish: a digest that doesn't match gives another round; the moved state is written before the answer; a lost answer
  gets the same key, also weeks later; the wrapped key opens only with the code's wrap key and that session; "done"
  only after `switched`.
- The new side: the unused rule (a first start's vault is taken and replaced; one log, token or upload in progress
  refuses); a keys folder with a key file that opens nothing is refused; a stop at every step of the switch, then a
  start, ends in the same data folder; the env rules of 3.4; `OFFER_MOVE` never ends a claimed move; `410` keeps the
  copy; cancel after `switching` refused.
- Old server moved: uploads `503 moved`, writes refused, the hourly line, undo, all kept across a restart.
- Export (built, `crates/moving/tests`): made from real files of every kind (logs, sidecars, counts, trust heads,
  records and journals, waiting markers, an upload in progress, tokens and a tie, the TLS key and certificate,
  `migrated.json`, a journal with a damaged length in the middle), then imported into a new folder and into one a
  first start made: every file the same bytes and times, and the token list, the stored logs, the upload's state, the
  chain report, the trust book and the certificate SHA-256 the same; the old recovery key still opens it; made while
  appends go on (a journal and its count stay of one moment); read again and again, and from many starts, always the
  same bytes; a flipped bit in the header and across the body, a cut anywhere, a header from another export, a wrong
  recovery key: refused with nothing in the data or keys folder changed; a token, a log or a foreign key file in the
  target: refused; not enough space found from the manifest; a newer `app` refused; an import stopped part way three
  times carries on; a switch stopped after each of its steps ends the same on the next start; a large folder (12,000
  files and a 32 MiB file) streamed in constant memory (the peak grows by about 2 MiB); made-up exports sealed with the
  right key but with a bad manifest (`..`, a dot entry at the top, `vault.json`, a temporary name, a path twice, out
  of walk order, a file inside another file's path, a name off the rule, entries that differ from the manifest, the
  wrong kind, bytes after the end): refused. By hand with the binary: an export of a running server, imported into
  a running new server (it switched in place) and into one not running; the same tokens, certificate SHA-256 and
  logs; an upload in progress finished on the new server; a switch that couldn't finish kept the server from
  starting until it could.

In one process (two servers on loopback, each with its own folders). Built, `crates/moving/tests/direct_move.rs`:

- A whole move while a test bridge uploads over TLS to the old server, then to the new one: everything acknowledged
  arrives, the bridge only ever sees `200`, `201` and `503` (one came during the pause: it waited at the gate and was
  told to try later, never `2xx` after the commit), the upload in progress finishes on the new server, the same
  tokens, certificate SHA-256 and data key, the old server told "done" once.
- Codes: none made (`no_move`, nothing staged), a typo (found before anything is sent), a wrong one (counted), an
  older new server (refused, the code still works), the right one, a second new server with it (`code_used`, with
  who and when), `move code` refused while a move is under way and `--replace` ending it (the first new server gets
  `cancelled` and keeps its copy), an expired code, and a new code from the same server carrying on with the kept
  copy to the end. Eleven wrong codes from one address: `slow_down`, ten counted.
- A fake server that answers the claim: `not_proven`, nothing staged. After a claim, another server at the old
  address (another certificate): no request reaches it, the problem is saved; the real old server back at its
  address: the move carries on and finishes.
- A proxy that cuts the first six connections part way, a 3 MB log and one bigger than a piece (17 MiB), the old
  server restarted after the claim and again mid-copy, the new one stopped mid-copy and run again: every file the
  same bytes and times.
- The moved state across a restart: uploads `503 moved`, `move code` and `move cancel` refused, another process
  can't get the gate, the new server's confirmation still arrives; `move undo` makes the running server take uploads
  again, and it stays undone after a restart.

- `move cancel` on the new server: the copy removed from the data and keys folders, the old server told (its move
  ends, it takes uploads as before), the new server unused again.
- The old server gone before the finish: `move finish --recovery-key` refuses a wrong key, then switches to what was
  copied; the vault opens with the new key file and with the old recovery key; nothing is sent to the old server.

Still to add here: a stop at every step of the move's switch (the import's switch has that test, and the move uses
the same `bring_all_in`); a pause ended by its timers under a slow finish.

**Two real servers in Docker** (a script, run before a release and by hand; it needs Docker and the image built from
the tree; not written yet: the same steps were tried by hand with the binary, two servers on one machine):

1. A Docker network; the old container runs as user 568:568 (TrueNAS) with a read-only root, `cap_drop: [ALL]` and
   `no-new-privileges`, as the compose file does. Make two tokens with `docker exec`.
2. A small test uploader (built from the test helpers, synthetic signed logs, made-up keys) sends a log every few
   seconds for two cars, all through the test.
3. `docker exec old /sentry-mate move code` gives the code. The new container starts with empty volumes and the two
   lines.
4. During the copy: `docker network disconnect` for 30 s; `docker restart` of the old one; `docker kill` of the new
   one and a start again.
5. Wait for `move: finished` in the new server's log and `move: done` in the old one's. Point the uploader at the new
   server.
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
11. Same machine: the new container with `10101:10100` and `SENTRY_MATE_MOVE_FROM=127.0.0.1` (host network): the port
    note in its log.

Afterwards the containers, volumes and test images are removed, and the build cache pruned.

## 10. Build order

1. **vault** (built): a vault's recovery part (`Vault::recovery_part`), opened with the recovery key
   (`RecoveryPart::open`), and a vault written somewhere with a given recovery part and a new key file
   (`Vault::install`); the chunks of a long stream sealed one by one with nonces made from their bytes
   (`vault::chunks`, for an export read from any byte and an import carrying on); the end of a records file's last
   whole record, from its lengths only (`records::whole_end`).
2. **gate** (built): the door and the gate, closing it, the server's `.lock`.
3. **ingest** (built, the gate in): every upload's tie, chunk and trust record in one pass, in a task of its own;
   `503 moving` with `Retry-After` after 20 s, and the gate's own answer when it is closed (`503 moved`, on every
   upload path); `cert.pem` written under a temporary name and renamed; the upload port's connections end with it;
   the TLS exporter value (`ingest::Exporter`, taken once after each handshake); the last call a closed gate refused
   (`Uploads::last_refused`, for the hourly line).
4. **moving** (built): the walk, the export, the import (`tree`, `export`, `import`); the code (`code`); the
   protocol, proofs, frames and digest (`proto`); the old side (`old`: `.move/`, the routes, pause, finish, the moved
   state, undo); the new side (`pull`: `.moving/`, the claim, the rounds, the finish, the same switch as the import,
   `bring_all_in`); the client with the pin (`client`).
5. **sentry-mate** (built): start-up order (`.lock`, the clean-up, `.importing/`, `.moving/`, then the vault), the
   env lines and `SENTRY_MATE_OFFER_MOVE`, `health` during a move, the moved state (no housekeeping, writes refused),
   `move code|status|cancel|undo|from|finish`, telling the old server after a move in.
6. **Docs**: README (Move to another server, Export and import), the TrueNAS guide (the two lines, the same-machine
   port), the compose file comments.
7. **Tests** of section 9, with the Docker script.
8. **Wave 2**: the `/api/v1` endpoints of section 6 and the three UI screens.

Not in this plan: moving only some cars, or merging two servers into one; pushing from the old server to a new one that
can't be reached (use the export file); moving a TPM-sealed key file (when TPM sealing comes, a move writes a new one
on the new machine the same way).

## Review notes

A design review (2026-11-17) found the problems below. Each was checked against the code.

1. **A writer that waited for the gate went ahead after the commit; the 60 s timer could end the pause under a slow
   finish.** Real. Writers check again after they hold the gate (section 1); the commit sets "moved" in memory first
   and keeps the gate exclusive while moved (2.5 step 4, 2.6); a finish or list in progress stops the timers (2.5);
   the test of a writer blocked during finish is in section 9.
2. **`flock` lets new shared locks past a waiting exclusive one, so a pause can starve.** Real (Linux grants a shared
   lock whenever only shared locks are held). The door lock is in section 1 and built in crate `gate`, with a lock in
   memory that lets a waiting pause go first inside the server.
3. **Nothing opened a half-done copy; "done" came before the new server switched; session rules clashed.** Real. The
   recovery part travels in the claim answer under `server_proof` and is kept in `.moving/` with a vault file of its
   own (2.3, 3.2); the finish answer is saved with `switching` before the switch (2.5 step 5); the old server keeps the
   session and answers until undo (2.3, 2.6); the session is made from the claim, so a lost claim answer gets it again
   (2.3); "done" only after `switched` (2.5 step 6); `move finish --recovery-key` (2.5, section 5). Found on the way:
   the plan kept only the auth key's SHA-256 on the old server, which can't check an HMAC; it keeps the auth key,
   sealed (2.1).
4. **`OFFER_MOVE` and "a new code ends any move" cancel a move under way.** Real. `OFFER_MOVE` makes a code only when
   there is none or it expired unclaimed; only `move code --replace` or `move cancel` end a claimed move; `410` keeps
   the copy, and a later code from the same server carries on (2.1, 3.4).
5. **Pieces of sealed and plain files could mix versions or keep a hole; the racy list wasn't kept; the digest didn't
   see content.** Real. Every want names its version (2.2); files are whole in `.moving/part/` before they are renamed
   and timed (2.4); what isn't settled is in the sealed state (2.4); the digest has the file id (2.5). Not taken: a
   sealed index of every file, because the copy itself says each file's version (whole files only, with the old
   length, time and file id), and an index of a million files would be rewritten every round.
6. **A records file cut at the first length that doesn't read hides damage the db would report.** Real
   (`db/src/journal.rs` reads past the last whole record and reports what isn't torn or zeros). In the paused round
   and in an export, records files are taken whole as stored (section 1, 4.2).
7. **The import had no state, so it couldn't carry on or finish a switch after a restart.** Real. `.importing/` has a
   vault of its own and a sealed state with the place reached at a chunk boundary and the running hashes; `switching`
   comes before the switch's first step (4.4, 3.3). Built.
8. **The importer couldn't split the bytes without holding the whole manifest.** Real. Each entry comes again before
   its bytes, and two running hashes check the copies agree (4.1). Built, with a test that streams a large folder in
   constant memory.
9. **No one rule for what is carried.** Real (an unreadable `lost+found`, Synology's `@eaDir/`). One function, used
   everywhere; what fails it is skipped on both sides and named (section 1). Built (`moving::tree`).
10. **Walk order wasn't byte order and no comparator was named.** Real (`tokens/` and `tokens.json` both exist while
    `Tokens::open` moves the old file). Defined part by part, one comparator (section 1). Built and tested.
11. **A time in the future looked racy for ever.** Real. Racy only when `0 <= read_at - mt < 2 s` (section 1).
12. **The forced pause after 10 rounds could hold uploads for 15 minutes again and again.** Real. No forced pause; the
    digest comes from the one walk at the pause's start, with heads kept from earlier rounds, so the pause walks once
    (2.4, 2.5). Not taken: writers recording every path they change, so the paused round lists only those. The paused
    round still needs a whole walk for the digest, and with heads kept that walk only stats; asking every writer of
    every crate to report paths is a rule each new crate could forget, for little gain.
13. **`move cancel` after the switch would remove the only copy.** Real. Refused from `switching` on; the API answers
    `409 finished` (section 5, section 6).
14. **On one machine the new app can't take port 10100.** Real. The tip, the log line and a Docker test step say to use
    another host port during the move (What the owner does, 3.5, section 9).

Found while building the export (2026-11-17):

15. **An export sealed with the chunk number as its nonce could seal two plaintexts with one nonce.** A growing file
    is linked into the snapshot; a writer whose append failed cuts what that write left and writes over it, inside
    the linked length. A resumed download would then seal different bytes under the same key and nonce. The export's
    chunks take their nonce from their own bytes instead (4.1).
16. **`cert.pem` was written in place.** A copy could see half a certificate. It is written under a temporary name and
    renamed, like every other file (3, section 10).
17. **A request dropped by its time limit could leave a write running outside the gate** (the blocking part of the
    write goes on after the request's future is dropped). An upload's writes run in a task of their own that holds
    the pass (section 1).
18. **A server whose switch failed part way would have started anyway**, on the first start's vault with the
    import's files beside it (and its housekeeping could then have written there). It finishes the switch first, or
    waits and tries again (3.3).
19. **The move's part files: the order of "time, sync, rename" wasn't fixed.** It is now (2.4).

Found while building the direct move (2026-11-18):

20. **A check from SHA-256 finds a typo only nearly always.** A 20-bit hash lets one typo in a million through. The
    code's check is a CRC instead: every wrong symbol and every swap of two side by side is found (2.1, tests in
    `moving::code`).
21. **A new server that lost the finish answer would never finish.** After a restart it catches up and asks for a
    pause again, but a moved server has no pause to give, so its lists never said "paused" and the last round never
    began. A moved server answers `pause` with `moved`, and its lists say `paused: "moved"` (nothing writes there):
    the last round runs, and the finish gives the same answer again.
22. **A records file's catch-up in pieces could end inside a record.** A 16 MiB piece, or the end of a 64 MiB answer,
    can cut a record. Outside the pause the copy keeps whole records only (cut back to the last whole one) and goes
    on from there; only bytes the old server listed as whole that aren't make it fetch the file whole again.
23. **A network failure was taken for a disk failure.** Reading a fetch answer and writing the copy both fail with an
    I/O error; told apart, a cut connection is tried again on a new one and a full disk waits for room.
24. **A connection was thrown away after every fetch.** The answer's end frame came before the chunked body's end, so
    the body was never read to its end. It is now, and a byte after the end frame is an error.
25. **`token list` and `fingerprint` hung a minute on a moved server**, waiting for the gate it holds for good. On a
    moved server they read without the gate (nothing writes there).
26. **A cancel could make `.moving/` again.** A write racing `move cancel` would make the folders it writes into
    again. The copy writes under `.moving/lock` (shared), the cancel removes the folder under it (exclusive), and the
    state is written without making folders.
27. **Waiting for the owner kept the new server down.** After a wrong or used code it now starts as usual (no data,
    no tokens, so nothing can upload to it) and says what to do; the next start with a new code moves.
28. **axum's query parser and hyper's client would add crates.** The list's two query fields are read by hand, and the
    new server's client is a small blocking HTTP/1.1 client over rustls.

Found by a review of the built move (2026-11-18), each checked against the code and fixed, with a test:

29. **A cancel made during a finish was overridden.** The commit wrote the moved state from the code record the
    request read when it was let in, so a `move cancel` or `move code --replace` landing between the two lost (and
    the new code was removed). The commit reads the record again under `.move/lock` and refuses with `410 cancelled`
    unless the same claim and session are still there and not cancelled (2.5 step 4).
30. **A fetch the new server stopped reading held the pause for ever.** The answer counted as busy and busy stopped
    both timers; with no write time-out, the old server answered every upload `503 moving` until the connection died.
    Each megabyte must be taken within 60 s, the 15 minutes hold whatever is busy, and a stalled answer's end isn't
    hearing from the new server (2.5 Timers).
31. **A disk that rounds file times never finished a move.** Every copied file looked changed, every round fetched
    everything, every digest differed, and the old server paused its uploads again every minute. The new server
    checks its disk before the claim (3.1).
32. **One file the old server's user can't open stopped the move and the export.** A root-owned 0600 file made every
    list page that reached it fail with `500`, tried again for ever, and the export failed as a whole. Such a file is
    skipped and named like an unreadable folder, in the list, the digest and the export alike (section 1).
33. **The moved state on disk and in memory could disagree.** The finish wrote the commit on a blocking thread and
    held the gate after the await: a dropped request or the ticker in between left the old server moved on disk but
    not held, so a later `move undo` was never seen. The commit and the hold are one step under the pause's lock; the
    ticker never takes a moved state for a cancel, and holds one it finds unheld (2.5 step 4).
