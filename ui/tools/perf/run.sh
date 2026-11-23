#!/usr/bin/env bash
# Start BIN on this folder's seeded data, wait for the engine, run the harness, stop the server.
#   ./run.sh /path/to/sentry-mate LABEL [extra measure.js options]
set -euo pipefail
P="$(cd "$(dirname "$0")" && pwd)"
BIN="$1"; LABEL="${2:-run}"; shift 2 || true
D="${PERF_DATA:-$P/data}"; K="${PERF_KEYS:-$P/keys}"
# a server left on this data folder (matched by its own arguments, never this script's)
pkill -f -- "--data $D --keys $K serve" || true
while pgrep -f -- "--data $D --keys $K serve" >/dev/null; do sleep 1; done
"$BIN" --data "$D" --keys "$K" serve --bind 127.0.0.1 > "$P/server-$LABEL.log" 2>&1 &
SRV=$!
trap 'kill $SRV 2>/dev/null || true; wait $SRV 2>/dev/null || true' EXIT
until curl -sk -o /dev/null https://127.0.0.1:10101/health; do sleep 1; done
node "$P/settled.js" --base https://127.0.0.1:10101
TMPDIR="$P" node "$P/measure.js" --label "$LABEL" --out "$P/$LABEL.json" --md "$P/$LABEL.md" "$@" | tee "$P/$LABEL.log"
