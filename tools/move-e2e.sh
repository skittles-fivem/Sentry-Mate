#!/usr/bin/env bash
# The move, end to end, in Docker: a direct move from an "old" server to a "new" one while a made-up CAN Bridge keeps
# uploading, then an export file brought into a third server, then (FAILURES=1) the ways a move can go wrong.
#
#   tools/move-e2e.sh                  run it (as root, or a user who may run docker and chown)
#   SKIP_BUILD=1 tools/move-e2e.sh     use the image sentry-mate:move-e2e as it is
#   FAILURES=1 tools/move-e2e.sh       also: a wrong code, an expired code, a short network cut and restarts of both
#                                      servers part way, a cut longer than the time-out, a new server with a full disk
#                                      (about 9 more minutes)
#   KEEP=1 tools/move-e2e.sh           leave the containers and folders for a look afterwards
#
# The image: Docker builds here may not reach crates.io, so the script copies the tracked files to $WORK/ctx, runs
# `cargo vendor` there (on this machine, which can reach crates.io) and builds the repo's Dockerfile from that copy
# with the vendored crates. vendor/ lives only in that copy: never commit it. `docker builder prune -f` afterwards.
#
# The made-up bridge (crates/moving/examples/test_bridge.rs) runs here, not in Docker, and reaches the containers on
# their network's addresses. Every log, key and token is made up.
set -euo pipefail

REPO=$(cd "$(dirname "$0")/.." && pwd)
WORK=${WORK:-${TMPDIR:-/tmp}/move-e2e}
IMG=sentry-mate:move-e2e
NET=mve2e-net
KEY=MADEUP0TESTCAR01 # a made-up car key
FAILED=0

say() { printf '\n== %s\n' "$*"; }
ok() { printf '   ok: %s\n' "$*"; }
bad() { printf '   FAILED: %s\n' "$*"; FAILED=$((FAILED + 1)); }
die() { printf '   FAILED: %s\n' "$*"; exit 1; }

teardown() {
  docker rm -f mve2e-old mve2e-new mve2e-third mve2e-x >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
  umount "$WORK/s/x/data" 2>/dev/null || true
  rm -rf "$WORK/s"
}
cleanup() { [ "${KEEP:-}" = 1 ] || teardown; }
trap cleanup EXIT

# --- build ----------------------------------------------------------------------------------------------------------
mkdir -p "$WORK"
if [ "${SKIP_BUILD:-}" != 1 ]; then
  say "image $IMG from $REPO (vendored)"
  rm -rf "$WORK/ctx" && mkdir -p "$WORK/ctx/.cargo"
  git -C "$REPO" ls-files -z | (cd "$REPO" && xargs -0 cp --parents -t "$WORK/ctx")
  (cd "$WORK/ctx" && cargo vendor --locked vendor 2> /dev/null > .cargo/config.toml)
  docker build -q -t "$IMG" "$WORK/ctx" >/dev/null
  rm -rf "$WORK/ctx"
fi
say "the made-up bridge"
(cd "$REPO" && cargo build -q --release --locked -p moving --example test_bridge)
TB="${CARGO_TARGET_DIR:-$REPO/target}/release/examples/test_bridge"
[ -x "$TB" ] || die "no $TB"

# --- helpers --------------------------------------------------------------------------------------------------------
teardown
mkdir -p "$WORK/s"
docker network create "$NET" >/dev/null

folders() { # name uid: empty data and keys folders owned by uid
  rm -rf "$WORK/s/$1" && mkdir -p "$WORK/s/$1/data" "$WORK/s/$1/keys" && chown -R "$2:$2" "$WORK/s/$1"
}
start() { # name uid [docker run options...]
  local n=$1 u=$2
  shift 2
  docker run -d --name "mve2e-$n" --network "$NET" --user "$u:$u" \
    -v "$WORK/s/$n/data:/data" -v "$WORK/s/$n/keys:/keys" "$@" "$IMG" >/dev/null
}
ip() { docker inspect -f "{{(index .NetworkSettings.Networks \"$NET\").IPAddress}}" "mve2e-$1"; }
sm() { local n=$1; shift; docker exec -i "mve2e-$n" /sentry-mate "$@"; }
waitlog() { # name pattern seconds
  local i
  for ((i = 0; i < $3; i++)); do
    docker logs "mve2e-$1" 2>&1 | grep -qE -- "$2" && return 0
    sleep 1
  done
  printf '   (no "%s" in the log of %s after %s s; its last lines:)\n' "$2" "$1" "$3"
  docker logs "mve2e-$1" 2>&1 | tail -15 | sed 's/^/   | /'
  return 1
}
ping_code() { "$TB" ping "$1" "$FP" "$TOKEN" | awk '{print $1, $2}'; } # "status code"
same_logs() { # name file-of-"i name sha" lines: every log there, byte for byte
  local n=$1 bad_ones=0 i name want got
  while read -r i name want; do
    got=$(sm "$n" log export "$name" | sha256sum | cut -d' ' -f1)
    [ "$got" = "$want" ] || { echo "   log $i $name: $got, wanted $want"; bad_ones=$((bad_ones + 1)); }
  done < "$2"
  [ "$bad_ones" = 0 ]
}
same_tokens() { [ "$(sm "$1" token list | awk '{print $1, $3}')" = "$TOKENS" ]; }

# --- the old server, with data --------------------------------------------------------------------------------------
say "old server (user 1000) with a token and uploads"
folders old 1000
start old 1000
waitlog old "certificate SHA-256" 30 || die "the old server didn't start"
FP=$(sm old fingerprint)
TOKEN=$(sm old token new --car "Test car" | grep -oE 'smt_[A-Za-z0-9_-]+')
OLD=$(ip old)
"$TB" send "$OLD" "$FP" "$TOKEN" "$KEY" 0 10 | grep '^done' | sed 's/^done //' > "$WORK/s/logs.txt"
[ "$(wc -l < "$WORK/s/logs.txt")" = 10 ] || die "the first 10 logs didn't all go up"
"$TB" part "$OLD" "$FP" "$TOKEN" "$KEY" 1000 > /dev/null || die "the part-done upload didn't go up"
TOKENS=$(sm old token list | awk '{print $1, $3}')
ok "10 logs (one of them 18 MB) and one part-done upload on the old server"

# --- (a) direct move -----------------------------------------------------------------------------------------------
say "(a) direct move, the bridge uploading all the way"
CODE=$(sm old move code | grep -oE 'SMMV(-[0-9A-Z]{5})+')
[ -n "$CODE" ] || die "no move code"
echo "$OLD" > "$WORK/s/addr"
"$TB" run "$WORK/s/addr" "$FP" "$TOKEN" "$KEY" 10 "$WORK/s/stop" > "$WORK/s/run.txt" 2>&1 &
RUN=$!
sleep 3
folders new 568
# a slow new server (a twentieth of a CPU, lifted once it copies), so uploads land on the old one meanwhile
start new 568 --cpus 0.05 -e SENTRY_MATE_MOVE_FROM=mve2e-old -e SENTRY_MATE_MOVE_CODE="$CODE"
waitlog new "proved the code" 60 > /dev/null || true
sleep 4
docker update --cpus "$(nproc)" mve2e-new > /dev/null
waitlog new "move: finished" 300 || die "the move didn't finish"
waitlog old "move: done" 60 && ok "old: move: done" || bad "the old server's log has no move: done"
NEW=$(ip new)
[ "$(ping_code "$OLD")" = "503 moved" ] && ok "the old server refuses uploads: 503 moved" \
  || bad "the old server answers $(ping_code "$OLD"), not 503 moved"
echo "$NEW" > "$WORK/s/addr" # the owner changes the address in the app
sleep 8
touch "$WORK/s/stop"
wait "$RUN" || true
grep '^done' "$WORK/s/run.txt" | sed 's/^done //' >> "$WORK/s/logs.txt"
N=$(wc -l < "$WORK/s/logs.txt")
grep -q "^done" "$WORK/s/run.txt" && ok "the bridge kept uploading: $N logs in all" || bad "no upload went through during the move"
grep -q "^503 moving" "$WORK/s/run.txt" && ok "uploads were held for a few seconds at the switch (503 moving)" || true
check_server() { # name what
  [ "$(sm "$1" fingerprint)" = "$FP" ] && ok "$2: same certificate SHA-256" || bad "$2: another certificate SHA-256"
  same_tokens "$1" && ok "$2: same tokens" || bad "$2: other tokens"
  same_logs "$1" "$WORK/s/logs.txt" && ok "$2: all $N logs byte for byte" || bad "$2: logs differ"
}
check_server new "new"
"$TB" send "$NEW" "$FP" "$TOKEN" "$KEY" 1000 1001 | grep '^done' | sed 's/^done //' > "$WORK/s/part.txt" || true
[ -s "$WORK/s/part.txt" ] && ok "new: the part-done upload finished with the old token" \
  || bad "new: the part-done upload didn't finish"
cat "$WORK/s/part.txt" >> "$WORK/s/logs.txt"
N=$(wc -l < "$WORK/s/logs.txt")

say "(a) restarts keep both states"
docker restart mve2e-old mve2e-new > /dev/null
sleep 5
waitlog new "certificate SHA-256" 60 > /dev/null || true
[ "$(ping_code "$OLD")" = "503 moved" ] && ok "old after a restart: still 503 moved" \
  || bad "old after a restart answers $(ping_code "$OLD")"
[ "$(ping_code "$NEW")" = "200 -" ] && ok "new after a restart: takes uploads" || bad "new after a restart answers $(ping_code "$NEW")"
check_server new "new after a restart"
docker stop mve2e-old > /dev/null

# --- (b) export file ------------------------------------------------------------------------------------------------
say "(b) export file from the new server into a third one"
# the old server's recovery key (a new vault writes it to its keys folder; the owner copies it away)
RK=$(sed -n 's/^ *\([^ ]\{20,\}\) *$/\1/p' "$WORK/s/old/keys/RECOVERY-KEY.txt" | head -1)
[ -n "$RK" ] || die "no recovery key"
folders third 568
docker exec mve2e-new /sentry-mate export > "$WORK/s/third/data/sentry-mate.smexport" || die "the export failed"
chown 568:568 "$WORK/s/third/data/sentry-mate.smexport"
ok "export: $(du -h "$WORK/s/third/data/sentry-mate.smexport" | cut -f1)"
start third 568
waitlog third "certificate SHA-256" 30 > /dev/null || die "the third server didn't start"
echo "$RK" | sm third import /data/sentry-mate.smexport > "$WORK/s/import.txt" 2>&1 || {
  sed 's/^/   | /' "$WORK/s/import.txt"; die "the import failed"; }
sleep 5
check_server third "third"
THIRD=$(ip third)
"$TB" send "$THIRD" "$FP" "$TOKEN" "$KEY" 2000 2001 | grep -q '^done' && ok "third: takes uploads with the old token" \
  || bad "third: an upload with the old token didn't go through"
docker restart mve2e-third > /dev/null && sleep 5
check_server third "third after a restart"
docker rm -f mve2e-third > /dev/null

# --- (c) failures ---------------------------------------------------------------------------------------------------
if [ "${FAILURES:-}" = 1 ]; then
  newcode() { # a fresh move code on the old server, ending any move under way there
    sm old move code --replace <<< yes 2>&1 | grep -oE 'SMMV(-[0-9A-Z]{5})+' | head -1
  }
  docker start mve2e-old > /dev/null
  waitlog old "certificate SHA-256" 30 > /dev/null || true
  echo yes | sm old move undo > /dev/null 2>&1 || bad "move undo failed"
  waitlog old "move: undone" 30 > /dev/null || bad "old: no word of move undo in its log"
  sleep 2
  [ "$(ping_code "$OLD")" = "200 -" ] && ok "old after move undo: takes uploads" || bad "old after move undo answers $(ping_code "$OLD")"
  docker rm -f mve2e-new > /dev/null

  say "(c) a mistyped code, and a well-formed code the old server never made"
  CODE=$(newcode)
  TYPO=${CODE:0:${#CODE}-1}$([ "${CODE: -1}" = 0 ] && echo 1 || echo 0)
  folders x 568
  start x 568 -e SENTRY_MATE_MOVE_FROM=mve2e-old -e SENTRY_MATE_MOVE_CODE="$TYPO"
  waitlog x "typo" 60 > /dev/null && ok "a typo: found on the new server, nothing sent" || bad "a typo: no clear message"
  docker rm -f mve2e-x > /dev/null
  folders x 568
  start x 568 -e SENTRY_MATE_MOVE_FROM=mve2e-old -e SENTRY_MATE_MOVE_CODE="$("$TB" random-code)"
  waitlog x "nothing was moved" 60 > /dev/null && ok "a wrong code: nothing was moved" || bad "a wrong code: no clear message"
  waitlog old "a wrong move code" 10 > /dev/null && ok "old: logs a wrong move code" || bad "old: no word of a wrong code"
  [ "$(ping_code "$OLD")" = "200 -" ] && ok "old still takes uploads" || bad "old answers $(ping_code "$OLD")"
  docker rm -f mve2e-x > /dev/null
  folders x 568
  start x 568 -e SENTRY_MATE_MOVE_FROM=mve2e-old -e SENTRY_MATE_MOVE_CODE="$CODE"
  waitlog x "move: finished" 120 > /dev/null && ok "the right code still works after a wrong one" || bad "the right code failed after a wrong one"
  docker rm -f mve2e-x > /dev/null
  echo yes | sm old move undo > /dev/null 2>&1 || true
  waitlog old "move: undone" 30 > /dev/null || true

  say "(c) an expired code"
  CODE=$(newcode)
  docker stop mve2e-old > /dev/null
  "$TB" expire-code "$WORK/s/old/data" "$WORK/s/old/keys" > /dev/null
  chown -R 1000:1000 "$WORK/s/old"
  docker start mve2e-old > /dev/null
  waitlog old "certificate SHA-256" 30 > /dev/null || true
  folders x 568
  start x 568 -e SENTRY_MATE_MOVE_FROM=mve2e-old -e SENTRY_MATE_MOVE_CODE="$CODE"
  waitlog x "nothing was moved" 60 > /dev/null && ok "an expired code: nothing was moved" || bad "an expired code: no clear message"
  docker logs mve2e-x 2>&1 | grep "nothing was moved" | sed 's/.*move: /   | move: /' | head -1
  docker rm -f mve2e-x > /dev/null

  say "(c) a network cut and a restart of the new server part way (a hundredth of a CPU while it copies)"
  "$TB" send "$OLD" "$FP" "$TOKEN" "$KEY" 57 58 | grep '^done' | sed 's/^done //' >> "$WORK/s/logs.txt"
  "$TB" send "$OLD" "$FP" "$TOKEN" "$KEY" 14 21 | grep '^done' | sed 's/^done //' > /dev/null
  CODE=$(newcode)
  folders x 568
  start x 568 --cpus 0.01 -e SENTRY_MATE_MOVE_FROM=mve2e-old -e SENTRY_MATE_MOVE_CODE="$CODE"
  waitlog x "proved the code" 120 > /dev/null || true
  sleep 2
  # a cut shorter than the client's time-out: the TCP connections live through it
  docker network disconnect "$NET" mve2e-x
  sleep 8
  docker network connect "$NET" mve2e-x
  sleep 2
  if docker logs mve2e-x 2>&1 | grep -q "move: finished"; then
    bad "the move finished before the restart: no restart part way was tried"
  else
    docker restart mve2e-x > /dev/null
    waitlog x "carrying on the move" 120 > /dev/null && ok "the new server restarted part way carried on" \
      || bad "no carrying on after a restart"
    docker logs mve2e-x 2>&1 | grep "carrying on the move" | sed 's/.*move: /   | move: /' | tail -1
    # the old server restarted part way: the new one's connections break and it has to connect again
    sleep 3
    docker restart mve2e-old > /dev/null
    waitlog x "connected to the old server again|move: finished" 180 > /dev/null \
      && ok "after the old server restarted part way, the new one connected again" \
      || bad "after the old server restarted part way, the new one didn't connect again"
  fi
  docker update --cpus "$(nproc)" mve2e-x > /dev/null
  waitlog x "move: finished" 300 > /dev/null && ok "finished after a cut and two restarts" \
    || bad "the move didn't finish after a cut and two restarts"
  docker logs mve2e-x 2>&1 | grep -E "WARN" | sed 's/.*WARN [a-z_:]*: /   | /' | sort -u | head -8 || true
  NOLD=$(cd "$WORK/s/old/data" && find . -type f -not -path './.*' -not -name vault.json -exec sha256sum {} + | sort -k2)
  NX=$(cd "$WORK/s/x/data" && find . -type f -not -path './.*' -not -name vault.json -exec sha256sum {} + | sort -k2)
  [ "$NOLD" = "$NX" ] && ok "after a cut and a restart: every sealed file the same as the old server's" \
    || bad "after a cut and a restart: the sealed files differ"
  docker rm -f mve2e-x > /dev/null
  echo yes | sm old move undo > /dev/null 2>&1 || true
  waitlog old "move: undone" 30 > /dev/null || true

  say "(c) a network cut longer than the new server's time-out (2 min)"
  CODE=$(newcode)
  folders x 568
  start x 568 --cpus 0.01 -e SENTRY_MATE_MOVE_FROM=mve2e-old -e SENTRY_MATE_MOVE_CODE="$CODE"
  waitlog x "proved the code" 120 > /dev/null || true
  sleep 3
  docker network disconnect "$NET" mve2e-x
  sleep 170
  docker network connect "$NET" mve2e-x
  docker update --cpus "$(nproc)" mve2e-x > /dev/null
  waitlog x "move: finished" 300 > /dev/null && ok "finished after a 3 minute cut" || bad "no finish after a long cut"
  docker logs mve2e-x 2>&1 | grep -q "lost the connection" && ok "the new server said it lost the connection" \
    || bad "the new server didn't notice the cut"
  docker logs mve2e-x 2>&1 | grep -q "connected to the old server again" && ok "and that it connected again" \
    || bad "no word of connecting again"
  # progress lines read "copying: 48 % (18.5 MB of 38.3 MB, 20 of 54 files)": never more files done than there are
  OVER=$(docker logs mve2e-x 2>&1 | sed -n 's/.*move: copying: .*, \([0-9]*\) of \([0-9]*\) files).*/\1 \2/p' \
    | awk '$1 > $2' | head -1)
  [ -z "$OVER" ] && ok "no file counted twice in the progress" || bad "the progress counts more files than there are: $OVER"
  docker rm -f mve2e-x > /dev/null
  echo yes | sm old move undo > /dev/null 2>&1 || true
  waitlog old "move: undone" 30 > /dev/null || true

  say "(c) a new server with too small a disk (a 20 MB data folder), then a bigger one"
  CODE=$(newcode)
  folders x 568
  # a 20 MB disk for the data folder, mounted here (the image has no mount program, so it grows from here too)
  mount -t tmpfs -o size=20m,mode=0755,uid=568,gid=568 tmpfs "$WORK/s/x/data"
  start x 568 -e SENTRY_MATE_MOVE_FROM=mve2e-old -e SENTRY_MATE_MOVE_CODE="$CODE"
  waitlog x "not enough space|disk is full" 120 > /dev/null && ok "the new server says its disk is too small" \
    || bad "a full disk: no clear message"
  [ "$(ping_code "$OLD")" = "200 -" ] && ok "old still takes uploads" || bad "old answers $(ping_code "$OLD")"
  # the owner gives it more room: the tmpfs grows in place
  mount -o remount,size=3g "$WORK/s/x/data"
  waitlog x "move: finished" 150 > /dev/null && ok "with room, it carried on by itself and finished" \
    || bad "with room, it didn't carry on"
  docker rm -f mve2e-x > /dev/null
  umount "$WORK/s/x/data"
fi

say "result"
[ "$FAILED" = 0 ] && echo "   all passed" || { echo "   $FAILED failed"; exit 1; }
