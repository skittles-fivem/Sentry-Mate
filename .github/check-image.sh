#!/usr/bin/env bash
# Checks a Sentry Mate image the way it is meant to run: no capabilities, a
# read-only root, no-new-privileges, and fresh data and keys folders that
# belong to the user it runs as. First it checks that folders it can't keep
# are refused with what to do: one of root's, none mounted, the keys not
# mounted, the keys inside the data on the server, the same folder at both.
# The Image workflow runs it on every image before it is tagged; run it by
# hand the same way:
#
#   .github/check-image.sh sentry-mate:check          (as the image's 1000:1000)
#   .github/check-image.sh sentry-mate:check 568      (as 568:568, like TrueNAS)
#
# EXPECT_VERSION: text `--version` must show (the workflow: build-N, sha-...).
# CHECK_PORT: the port on 127.0.0.1 it uses (10100).
# Needs docker, curl, openssl, and sudo when not run as root.
set -euo pipefail

image="${1:?usage: check-image.sh IMAGE [UID]}"
id="${2:-1000}"
port="${CHECK_PORT:-10100}"
name="sentry-mate-check-$$"
sudo=""
if [ "$(id -u)" -ne 0 ]; then sudo=sudo; fi
dir="$(mktemp -d)"

cleanup() {
  docker rm -f "$name" >/dev/null 2>&1 || true
  $sudo rm -rf "$dir"
}
trap cleanup EXIT

fail() {
  echo "::error::$*"
  docker logs "$name" 2>&1 | tail -30 || true
  exit 1
}

# as the compose file runs it; --user only for another user than the image's own
user=()
if [ "$id" != 1000 ]; then user=(--user "$id:$id"); fi
start() {
  docker run -d --name "$name" -p "127.0.0.1:$port:10100" --cap-drop ALL --read-only \
    --security-opt no-new-privileges:true "${user[@]}" \
    -v "$dir/data:/data" -v "$dir/keys:/keys" "$image" >/dev/null
}

# A start that must be refused: it stops with exit code 1, and its message
# has each TEXT.  refused TEXT... -- [docker run options]
refused() {
  local texts=()
  while [ "$1" != -- ]; do texts+=("$1"); shift; done
  shift
  docker run -d --name "$name" --cap-drop ALL --read-only --security-opt no-new-privileges:true \
    "${user[@]}" "$@" "$image" >/dev/null
  timeout 60 docker wait "$name" >/dev/null || fail "it did not stop within 60 s: it started"
  said="$(docker logs "$name" 2>&1)"
  echo "$said"
  [ "$(docker inspect -f '{{.State.ExitCode}}' "$name")" = 1 ] || fail "it did not stop with exit code 1"
  for t in "${texts[@]}"; do grep -qF -- "$t" <<<"$said" || fail "the message does not say: $t"; done
  docker rm "$name" >/dev/null
}

# fresh, empty data and keys folders of the user given (UID[:GID])
fresh() {
  $sudo rm -rf "$dir/data" "$dir/keys"
  mkdir -p "$dir/data" "$dir/keys"
  $sudo chown "$1" "$dir/data" "$dir/keys"
}

# nothing in the folder (a refused start wrote nothing)
empty() {
  [ -z "$($sudo ls -A "$1")" ] || fail "something was written to $1: $($sudo ls -A "$1")"
}

healthy() {
  for _ in $(seq 1 90); do
    [ "$(docker inspect -f '{{.State.Running}}' "$name")" = true ] || fail "it stopped"
    [ "$(docker inspect -f '{{.State.Health.Status}}' "$name")" = healthy ] && return 0
    sleep 1
  done
  fail "not healthy after 90 s"
}

served_fingerprint() {
  openssl s_client -connect "127.0.0.1:$port" -tls1_3 </dev/null 2>/dev/null \
    | openssl x509 -outform DER | sha256sum | cut -d' ' -f1
}

echo "== $image as $id:$id"
version="$(docker run --rm "$image" --version)"
echo "$version"
case "$version" in "sentry-mate "*"${EXPECT_VERSION:-}"*) ;; *) fail "--version shows no ${EXPECT_VERSION:-version}" ;; esac

echo "== a data folder of root's: refused, with what to do"
fresh "$id:$id"
$sudo chown 0:0 "$dir/data"
refused "runs as uid $id (group $id)" "sudo chown -R $id:$id" -- -v "$dir/data:/data" -v "$dir/keys:/keys"

echo "== no folders mounted: refused, with the lines to add"
refused "the data folder /data and the keys folder /keys are not mounted." \
  "would be lost when the container is replaced" "  - ./data:/data" "  - ./keys:/keys" \
  "  - /mnt/tank/apps/sentry-mate:/data" "  - /mnt/tank/apps/sentry-mate-keys:/keys" --
# every command that opens the vault, not only serve
if said="$(docker run --rm "${user[@]}" "$image" token list 2>&1)"; then fail "token list ran with nothing mounted"; fi
echo "$said"
grep -qF "are not mounted" <<<"$said" || fail "token list does not say the folders are not mounted"

echo "== the keys folder not mounted: refused, nothing written to data"
fresh "$id:$id"
refused "the keys folder /keys is not mounted." "the data would then open only with the recovery key" \
  "  - ./keys:/keys" "  - /mnt/tank/apps/sentry-mate-keys:/keys" -- -v "$dir/data:/data"
empty "$dir/data"

echo "== the keys folder inside the data folder on the server: refused"
fresh "$id:$id"
mkdir -p "$dir/data/keys"
$sudo chown "$id:$id" "$dir/data/keys"
refused "the keys folder /keys is a folder inside the data folder on the server (it is /data/keys here)" \
  "The key file would be in every backup of the data" -- -v "$dir/data:/data" -v "$dir/data/keys:/keys"
empty "$dir/data/keys"

echo "== the same folder at both: refused"
fresh "$id:$id"
refused "the keys folder /keys and the data folder /data are the same folder of the server" \
  -- -v "$dir/data:/data" -v "$dir/data:/keys"
empty "$dir/data"
$sudo rm -rf "$dir/data" "$dir/keys"

echo "== first start: fresh folders that belong to $id"
mkdir -p "$dir/data" "$dir/keys"
$sudo chown "$id:$id" "$dir/data" "$dir/keys"
start
healthy
docker logs "$name" 2>&1 | tail -5

echo "== TLS 1.3 answers, TLS 1.2 and plain HTTP are refused"
# (answers kept in variables, not piped into grep -q: a pipe cut short fails under pipefail)
code="$(curl -sk --tlsv1.3 -o /dev/null -w '%{http_code}' "https://127.0.0.1:$port/api/can/logs/ping")"
[ "$code" = 401 ] || fail "TLS 1.3 without a token: HTTP $code, not 401"
if curl -sk --tlsv1.2 --tls-max 1.2 -o /dev/null "https://127.0.0.1:$port/api/can/logs/ping"; then
  fail "TLS 1.2 answered"
fi
if curl -s -o /dev/null "http://127.0.0.1:$port/api/can/logs/ping"; then fail "plain HTTP answered"; fi

echo "== the recovery key file is in keys, not in the log"
$sudo test -s "$dir/keys/RECOVERY-KEY.txt" || fail "no RECOVERY-KEY.txt in keys"
[ "$($sudo stat -c '%u %a' "$dir/keys/sentry-mate.key")" = "$id 600" ] || fail "the key file is not $id's, mode 600"
rk="$($sudo sed -n 's/^  \([^ ]\)/\1/p' "$dir/keys/RECOVERY-KEY.txt" | head -n 1)"
[ -n "$rk" ] || fail "no recovery key in RECOVERY-KEY.txt"
logs="$(docker logs "$name" 2>&1)"
if grep -qF "$rk" <<<"$logs"; then fail "the recovery key is in the log"; fi

echo "== the certificate is the one \`fingerprint\` prints"
fp="$(docker exec "$name" /sentry-mate fingerprint)"
[ "$(served_fingerprint)" = "$fp" ] || fail "served certificate is not $fp"
echo "$fp"

echo "== token new and token list (docker exec), the token works"
out="$(docker exec "$name" /sentry-mate token new --car "Check car")"
token="$(sed -n 's/^  \(smt_[A-Za-z0-9_-]*\)$/\1/p' <<<"$out")"
[ -n "$token" ] || fail "token new printed no token"
list="$(docker exec "$name" /sentry-mate token list)"
echo "$list"
grep -q "Check car" <<<"$list" || fail "token list does not show the car"
ping_api() { curl -sk --tlsv1.3 -H "Authorization: Bearer $token" "https://127.0.0.1:$port/api/can/logs/ping"; }
grep -q '"ok":true' <<<"$(ping_api)" || fail "the new token is refused"

echo "== it runs as $id with no capabilities and no_new_privs"
pid="$(docker inspect -f '{{.State.Pid}}' "$name")"
st="$($sudo grep -E '^(Uid|Gid|CapPrm|CapEff|CapBnd|NoNewPrivs):' "/proc/$pid/status")"
echo "$st"
grep -Eq "^Uid:\s+$id\s+$id\s+$id\s+$id$" <<<"$st" || fail "not uid $id"
grep -Eq "^Gid:\s+$id\s+$id\s+$id\s+$id$" <<<"$st" || fail "not gid $id"
for c in CapPrm CapEff CapBnd; do grep -Eq "^$c:\s+0+$" <<<"$st" || fail "$c is not empty"; done
grep -Eq '^NoNewPrivs:\s+1$' <<<"$st" || fail "no_new_privs is off"

echo "== docker stop: a clean stop within 10 s"
t0="$(date +%s%N)"
docker stop "$name" >/dev/null
ms=$(( ($(date +%s%N) - t0) / 1000000 ))
echo "stopped in $ms ms"
[ "$(docker inspect -f '{{.State.ExitCode}}' "$name")" = 0 ] || fail "exit code not 0 (killed?)"
[ "$ms" -lt 10000 ] || fail "took $ms ms"

echo "== restart: the same certificate, the vault opens with nothing to type, the token still works"
docker start "$name" >/dev/null
healthy
[ "$(served_fingerprint)" = "$fp" ] || fail "the certificate changed"
logs="$(docker logs "$name" 2>&1)"
[ "$(grep -c 'new vault made' <<<"$logs")" = 1 ] || fail "a second vault was made"
grep -q '"ok":true' <<<"$(ping_api)" || fail "the token is refused after the restart"

echo "== all checks passed ($image as $id:$id)"
