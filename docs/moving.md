# Moving to another server

[← Back to the README](../README.md)

This guide moves everything from one Sentry Mate server to another: the CAN logs, uploads still arriving, the
upload tokens, the trust records and the TLS key. The CAN Bridge keeps its token and its certificate SHA-256. At
most you change the server address in the app.

There are two ways:

- **Direct move** (the usual way). The new server copies everything from the old one over the network. The old one
  keeps taking uploads while it copies. At the end the new server takes over by itself. Nothing to press.
- **Export file.** One file with everything in it. It opens on the new server with the recovery key. Use it when the
  two servers can't reach each other, or to keep a copy somewhere.

The addresses and codes below are made up. Use your own.

## From the web pages (no shell, no commands)

The pages and their API are in `docs/ui-api.md`, "Moving and first-run setup". The pages are on the HTTPS address of
the server only: the set-up code, passwords, move codes and recovery keys never go over plain HTTP.

### Which way to choose

- **Move code**: both servers are on at the same time and can reach each other (the same home network, or the new
  one can reach the old one's upload port). The old server keeps taking uploads while the new one copies, and the
  new one takes over by itself at the end.
- **Export file**: the servers can't reach each other, or the old one will be off, or you want a copy to keep. One
  file with everything; it opens only with the recovery key and never holds the key file.

### On the old server: make a move code

1. Sign in as an admin. Open **Settings → Server and access → Move to another server**.
2. Press **Make a move code** and type your password again. The code shows in large letters, with **Copy** and the
   time it stops working: it works once, for 24 hours, and only from your home network unless you tick **from
   anywhere**. It is shown this once; if you lose it, make a new one (the old one stops working).
3. Keep the page open, or come back to it: once the new server uses the code, it shows what is being copied (how
   much, how many files, which step). At the end it says **Moved to 192.0.2.30: you can turn this server off**.

After the move the old server's pages stay up, read only: you can look at everything, sign in and out, download an
export file and undo the move, but nothing else changes there (any change answers "moved").

**Cancel the code** stops the code working, or ends a move that has not finished. The old server's data is never
changed by a move, so cancelling is always safe.

### On the new server: the first page

Install Sentry Mate with empty `data` and `keys` folders and open its address. The first page asks for the
**set-up code**: the server prints it in its log on every start until it is set up. On TrueNAS: **Apps**, the Sentry
Mate app, **Logs**; look for the line `first-run set-up code: SETUP-...`. (With plain Docker: `docker logs
sentry-mate`.) Only someone who can read the log has it, so whoever reaches a fresh server first can't take it over.

Then choose one of three:

- **Start fresh**: a new server with nothing in it. Make the admin account. The page then shows the **recovery key**
  once, with **Copy** and **Download**: keep it in a password manager or on paper, away from the server. Tick **I have
  saved it** to go on (the server then deletes its copy of the key file). Closed the page before the tick? Nothing is
  lost: **Settings → Server and access** has **Show the key again** (your password first) until it is ticked, and
  **Make a new recovery key** for a key lost later (the old one stops working; exports made before still need it).
- **Move here from another server**: type the old server's address (the one the CAN Bridge sends to, for example
  `192.0.2.20`) and the move code. The page shows the copy as it goes (the server keeps its pages up while it
  copies). At the end the server switches to the data and starts again by itself (a few seconds); then sign in with
  your account from the old server: everything is there, accounts too.
- **Restore from an export file**: choose the file and type the old server's recovery key. The key and the file are
  checked before the upload starts; then the page shows the upload and the checks. The server reads the file as it
  arrives, so it needs room for the data only, not for the file as well. At the end the server switches to the data
  and starts again by itself (a few seconds); then sign in with your account from the old server.

**If it stops half way**: nothing of the old server is changed. A move that loses the connection tries again by
itself and carries on where it stopped; the page says what it waits for. An upload that stopped (the browser was
closed, the network dropped) carries on when you choose the same file again. If the old server is gone for good
while a move copies, the page offers **Finish with what was copied**: it needs the old server's recovery key and a
tick that the old server is gone for good (the last changes may be missing). A move cancelled on the old server stops
and waits: make a new move code there, type it in **New move code from the old server** and press **Carry on**; what
was copied is kept. The same goes for a move the old server no longer knows (its move was removed there) or one where
the old server could not prove itself. Carrying on takes a code from the same old server only. **Give up** removes what was copied, and the page goes back to the three choices.

### On the old server: download an export file

**Settings → Server and access → Download export file**, and your password again. The server prepares the file in
three steps, shown with a bar, how much of how much, the speed and the time left: it counts what it will carry, waits
for the writes under way, then takes the snapshot (uploads wait for this step; it takes seconds unless the database is
large). Then the browser downloads it; a cut download carries on. You can close the page and come back: the file is
still being made. **Cancel** stops it. If nothing moves for a minute the page says so; if it fails the page says why
and what to do. Once the file is ready, the page shows its size and, a little later, its SHA-256: check a copy with
`sha256sum` before you rely on it. The file
is kept for a day after it was made or last downloaded, and a week at most. On the new server, choose **Restore
from an export file** on the first page.

### Undo the move

On the old server, **Settings → Server and access → Undo the move** (your password again). **Stop the new server
first**: if both take uploads, they drift apart. The old server takes uploads again at once.

### The CAN Bridge

Coming from Sentry Six Manager rather than from another Sentry Mate? Its CAN logs come over with its CAN export
bundle and Settings → Import: [Bringing in the logs Sentry Six Manager already
has](cars-and-bridge.md#bringing-in-the-logs-sentry-six-manager-already-has).

The new server makes upload tokens in **Settings → Cars**, and shows the three fields of the app's "Second Sentry
Six Manager" section ready to copy: the server address, the token and the certificate SHA-256. After a move or a
restore the bridge keeps its token and its certificate SHA-256; change only the server address, if it changed (see
"In the CAN Bridge app" below).

## From the command line

The same as the pages, for a server you reach with a shell.

### Before you start

- The new server runs the **same version of Sentry Mate as the old one, or a newer one**. If it is older, the move
  says so and nothing is used up: update it and start it again.
- The new server has **empty `data` and `keys` folders**. A server that was only started once, with nothing uploaded
  to it, is fine too.
- The new server's data folder is on a disk that **keeps exact file times**. ZFS (TrueNAS), ext4, xfs and btrfs do.
  If the disk doesn't, the move stops before it begins and says so (see the end of this guide).
- Have the **recovery key** of the old server somewhere safe. A direct move doesn't ask for it, but the export file
  and the last resort below do.

### Direct move on TrueNAS (Install via YAML)

No shell is needed.

1. **Old server: get a move code.** In Apps, edit the old Sentry Mate app and add one line under `environment:`:

   ```yaml
         - SENTRY_MATE_OFFER_MOVE=yes
   ```

   Save. When the app has restarted, open its **Logs**. A line shows the move code, like
   `SMMV-1K3QX-7WD2M-ZP4HT-9BN6R-XC8VA-5JE0F`. Copy it. The code works once, for 24 hours, and only from your home
   network. For a new server somewhere else (another house, a rented server), write `anywhere` instead of `yes`.

2. **New server: install it with two more lines.** Install Sentry Mate as a new app, the same way as the old one, with
   empty `data` and `keys` folders. Add these two lines under `environment:`:

   ```yaml
         - SENTRY_MATE_MOVE_FROM=192.0.2.20
         - SENTRY_MATE_MOVE_CODE=SMMV-1K3QX-7WD2M-ZP4HT-9BN6R-XC8VA-5JE0F
   ```

   The first is the old server's address: the one the CAN Bridge uses. The second is the code from step 1.

3. **Watch the new server's log.** It shows the copy as it goes (`move: copying: 50 % ...`). It ends with
   `move: finished: this server holds the data now and serves it`.

4. **Watch the old server's log.** It says `move: done: ... You can stop it now`. Only then stop the old app.

5. **The CAN Bridge app.** If the new server has another address, change it (see "In the CAN Bridge app" below).

6. **Tidy up (optional).** Remove `SENTRY_MATE_OFFER_MOVE` from the old app, and the two lines from the new app. They
   do nothing once the move is done.

#### Same machine, new disk or pool

The old app holds ports 10100 and 10101, so the new app can't have them during the move. Give the new app other host
ports for now, for example `10200:10100` and `10201:10101`, and use the machine's own address in
`SENTRY_MATE_MOVE_FROM`. After the old server's log says `move: done`, stop the old app, set the new app's ports back to
`10100:10100` and `10101:10101` and redeploy. The CAN Bridge then needs no change at all.

### Direct move with plain Docker or compose

1. **Old server:**

   ```bash
   docker exec sentry-mate /sentry-mate move code
   ```

   It shows the code once. Add `--anywhere` for a new server outside your home network.

2. **New server:** put the two lines in its compose file, as in the TrueNAS steps, and start it. Or start it as
   usual and run:

   ```bash
   docker exec sentry-mate /sentry-mate move from 192.0.2.20 SMMV-1K3QX-7WD2M-ZP4HT-9BN6R-XC8VA-5JE0F
   ```

   The command checks the code at once and shows the progress. Closing it doesn't stop the move.

3. As above: wait for `move: finished` on the new server and `move: done` on the old one.

`docker exec sentry-mate /sentry-mate move status` shows what a move is doing, on either server.

### Export file

1. **Old server:** make the file. It works while the server runs (uploads wait a few seconds).

   ```bash
   docker exec sentry-mate /sentry-mate export > sentry-mate.smexport
   ```

   With compose, add `-T`: `docker compose exec -T sentry-mate /sentry-mate export > sentry-mate.smexport`.

2. **Copy the file** into the new server's data folder. The server's user must be able to read it (user 1000, or 568
   on TrueNAS).

3. **New server:** start it with empty folders, then:

   ```bash
   docker exec -it sentry-mate /sentry-mate import /data/sentry-mate.smexport
   ```

   It asks for the old server's recovery key. It checks the whole file, and only then switches to it. If it stops part
   way, run it again: it carries on.

The file never holds the key file. Without the recovery key it is useless to anyone who takes it. Delete it once the
new server runs.

## One server at a time: what moves, what stops, what starts

A direct move is one to one: the new server takes over everything the old one did, and the old one stops all of it.
At no moment do both read the cars or publish for them.

**What moves.** The whole data folder, exactly as stored: the CAN logs and uploads still arriving, the tokens, the
trust records, the certificate, the history, the accounts and the audit log, the settings, and the state of every
input: the Tesla API and TeslaFi accounts with their saved waits (a back-off, Tesla's or TeslaFi's "wait until"),
their pacing and the day's request count, how far the Supercharger invoices were read, the local TeslaMate's
settings and its catch-up mark, the MQTT settings, and the imports with their uploads.

**What stops on the old server, and when.** Uploads stop at the last-round pause: they wait at the old server's
write gate while the last changes are copied (the bridge keeps its logs and sends them again). Everything else stops
at the switch, the moment the old server checks the copy and records that the data moved:

- the Tesla API, TeslaFi and local TeslaMate inputs (no more requests, the MQTT subscription closed);
- an import or a check under way: it stops at its next read and writes nothing more (its job stays as it was);
- MQTT publishing: it closes its connection and clears what it published (an empty retained message on every car's
  value topics, its Home Assistant discovery configs and its status topic), so the broker never holds two servers'
  values for the same topics;
- the history's own work: decoding, a full rebuild (it carries on on the new server), the GPS layout fit,
  compaction, the scan for missing logs, the full rebuild's watch, the invoices' audit and the upkeep (the
  accounts' "last seen", the connections overview).

The old server answers the new one's finish only after all of that has stopped (at most 30 seconds); the new server
starts its own inputs only once it has that answer. The old server's log, and the move's log on its page, say
`Inputs stopped on this server: ...`. From then on it writes nothing to its data folder; its pages stay up, read
only. A restart of it starts none of these again. Signing in and out still works there (a session is the one
thing it records).

**What starts on the new server.** On its first start after the switch it starts every input the old one had, from
where the old one left it: a wait still running is waited out (no request is sent before it ends), the day's count
goes on, an import cut by the move is `ready` to run again (running it again replaces what it stored, never doubles
it), and the local TeslaMate catches up from its mark. Nothing that reached the old server's data before the switch
is fetched or published twice, and nothing is lost: what an input fetched in the moment between the last check and
the stop was not carried, so the new server fetches it again from the mark that was. Its move log says
`Inputs started here: ...`.

**During the pause** the inputs still run on the old server. A change one of them writes then makes the copy differ;
the new server copies what changed again (the log says so), and after five tries it lets the pause end and asks again.

**One exception**: `move finish --recovery-key` (the old server gone for good) switches without the old server, so it
stops nothing there. Never start that old server again with its inputs or the CAN Bridge pointed at it.

## In the CAN Bridge app

- **Same address** (the new server took the old one's address, or the same machine with port 10100 back): change
  nothing.
- **Another address:** in **Second Sentry Six Manager**, change only the server address to the new server, for example
  `https://192.0.2.30:10100/api/can/logs`. Keep the token and the certificate SHA-256 as they are.

Nothing is lost while you do it. The bridge keeps its logs and sends them once it reaches the new server.

## What the old server shows after

- Its log says where the data went and when: `this server's data moved to 192.0.2.30 on ...`.
- It takes no more uploads. It answers the CAN Bridge "moved, try later" (`503 moved`). The app shows that it will
  retry. Once an hour, while a bridge still calls, the log says which car key still sends there.
- Commands that change data refuse there. Reading still works (`fingerprint`, `token list`, `export`).
- Keep its folders for a while as a backup, then delete them.

## Undo with a command

On the old server:

```bash
docker exec -it sentry-mate /sentry-mate move undo
```

Type `yes`. The old server takes uploads again at once. **Stop the new server first**: if both take uploads, the two
drift apart. The old server's data was never changed by the move, so nothing else is needed.

## If it stops half way

The words below are the log's; the web pages show the same words, with what to do. Most problems fix themselves.
The new server never gives up on its own, and it tries again where that can help.

| What you see | What to do |
|---|---|
| `lost the connection` or `nothing answers at ...; trying again` | Nothing. It carries on where it stopped when the old server answers again. Check the address if it goes on. |
| Either server restarted | Nothing. The move carries on. |
| `the move code has a typo` | Check the code against the one the old server showed. Fix the line, restart the new app. |
| `didn't take this code` | Copy the code again from the old server's log (it shows the same code on every start until it is used). Fix the line, restart the new app. |
| `this move code expired` | Restart the old app (with `SENTRY_MATE_OFFER_MOVE` still there, it makes a new code), or run `move code` there. Put the new code in `SENTRY_MATE_MOVE_CODE`, restart the new app. |
| `this move code was used already` | If that wasn't your new server, run `move cancel` on the old server now. To move again: `move code --replace` on the old server, then the new code in the line. |
| `the old server runs a newer Sentry Mate` | Update the new server, start it again. The code still works. |
| `not enough space` | Make room on the new server's disk. It carries on by itself. |
| `keeps file times only roughly` | Put the new server's data folder on another disk (ZFS, ext4, xfs or btrfs), then start again. The code still works. |
| `changes faster than this copy catches up` | Something big runs on the old server (an import). Let it end first. |
| `the move was cancelled on the old server` | The copy so far is kept. With a new code from the same server it carries on from there. |
| The old server's log says `the pause ended ...; uploads go on` | Nothing. The new server catches up and tries the last step again. |
| The old server is gone for good (its disk died) | On the new server: `docker exec -it sentry-mate /sentry-mate move finish --recovery-key`. It asks for the recovery key and switches to what was copied. The last changes may be missing. Never start the old server again with the bridge pointed at it. |
| You want to give up | On the new server: `move cancel` (the copy is removed and the old server is told). On the old server: `move cancel` (the code stops working). |

Files the old server can't read (a folder like `lost+found`, a file copied in by root) are left out and named in both
logs: `... were skipped on the old server (not carried)`. Fix their owner on the old server if you need them, and the
next round copies them.
