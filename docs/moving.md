# Moving to another server

This guide moves everything from one Sentry Mate server to another: the CAN logs, uploads still arriving, the
upload tokens, the trust records and the TLS key. The CAN Bridge keeps its token and its certificate SHA-256. At
most you change the server address in the app.

There are two ways:

- **Direct move** (the usual way). The new server copies everything from the old one over the network. The old one
  keeps taking uploads while it copies. At the end the new server takes over by itself. Nothing to press.
- **Export file.** One file with everything in it. It opens on the new server with the recovery key. Use it when the
  two servers can't reach each other, or to keep a copy somewhere.

The addresses and codes below are made up. Use your own.

## Before you start

- The new server runs the **same version of Sentry Mate as the old one, or a newer one**. If it is older, the move
  says so and nothing is used up: update it and start it again.
- The new server has **empty `data` and `keys` folders**. A server that was only started once, with nothing uploaded
  to it, is fine too.
- The new server's data folder is on a disk that **keeps exact file times**. ZFS (TrueNAS), ext4, xfs and btrfs do.
  If the disk doesn't, the move stops before it begins and says so (see the end of this guide).
- Have the **recovery key** of the old server somewhere safe. A direct move doesn't ask for it, but the export file
  and the last resort below do.

## Direct move on TrueNAS (Install via YAML)

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
         - SENTRY_MATE_MOVE_FROM=192.168.1.20
         - SENTRY_MATE_MOVE_CODE=SMMV-1K3QX-7WD2M-ZP4HT-9BN6R-XC8VA-5JE0F
   ```

   The first is the old server's address: the one the CAN Bridge uses. The second is the code from step 1.

3. **Watch the new server's log.** It shows the copy as it goes (`move: copying: 50 % ...`). It ends with
   `move: finished: this server holds the data now and serves it`.

4. **Watch the old server's log.** It says `move: done: ... You can stop it now`. Only then stop the old app.

5. **The CAN Bridge app.** If the new server has another address, change it (see "In the CAN Bridge app" below).

6. **Tidy up (optional).** Remove `SENTRY_MATE_OFFER_MOVE` from the old app, and the two lines from the new app. They
   do nothing once the move is done.

### Same machine, new disk or pool

The old app holds port 10100, so the new app can't have it during the move. Give the new app another host port for
now, for example `10101:10100`, and use the machine's own address in `SENTRY_MATE_MOVE_FROM`. After the old server's
log says `move: done`, stop the old app, set the new app's port back to `10100:10100` and redeploy. The CAN Bridge
then needs no change at all.

## Direct move with plain Docker or compose

1. **Old server:**

   ```bash
   docker exec sentry-mate /sentry-mate move code
   ```

   It shows the code once. Add `--anywhere` for a new server outside your home network.

2. **New server:** put the two lines in its compose file, as in the TrueNAS steps, and start it. Or start it as
   usual and run:

   ```bash
   docker exec sentry-mate /sentry-mate move from 192.168.1.20 SMMV-1K3QX-7WD2M-ZP4HT-9BN6R-XC8VA-5JE0F
   ```

   The command checks the code at once and shows the progress. Closing it doesn't stop the move.

3. As above: wait for `move: finished` on the new server and `move: done` on the old one.

`docker exec sentry-mate /sentry-mate move status` shows what a move is doing, on either server.

## Export file

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

## In the CAN Bridge app

- **Same address** (the new server took the old one's address, or the same machine with port 10100 back): change
  nothing.
- **Another address:** in **Second Sentry Six Manager**, change only the server address to the new server, for example
  `https://192.168.1.30:10100/api/can/logs`. Keep the token and the certificate SHA-256 as they are.

Nothing is lost while you do it. The bridge keeps its logs and sends them once it reaches the new server.

## What the old server shows after

- Its log says where the data went and when: `this server's data moved to 192.168.1.30 on ...`.
- It takes no more uploads. It answers the CAN Bridge "moved, try later" (`503 moved`). The app shows that it will
  retry. Once an hour, while a bridge still calls, the log says which car key still sends there.
- Commands that change data refuse there. Reading still works (`fingerprint`, `token list`, `export`).
- Keep its folders for a while as a backup, then delete them.

## Undo

On the old server:

```bash
docker exec -it sentry-mate /sentry-mate move undo
```

Type `yes`. The old server takes uploads again at once. **Stop the new server first**: if both take uploads, the two
drift apart. The old server's data was never changed by the move, so nothing else is needed.

## If it stops half way

Most problems fix themselves. The new server never gives up on its own, and it tries again where that can help.

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
