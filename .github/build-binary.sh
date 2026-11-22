#!/usr/bin/env bash
# Builds the binary the image takes, the way the Dockerfile's `build` stage
# does (the same Rust image, read from the Dockerfile with its digest; the
# same `cargo build --release --locked -p sentry-mate`), but with a build
# folder kept between runs, so a push builds again only the crates it
# changed. The Image workflow runs it, then builds the image with
# build-contexts: binary=OUT, in place of the Dockerfile's two build stages.
#
#   .github/build-binary.sh CACHE OUT
#
# CACHE: the kept folder (target/ and cargo's home with the downloaded
#   crates; made when missing). OUT: made new, laid out like the
#   Dockerfile's `binary` stage: sentry-mate, out/data and out/keys (700).
# BUILD_NUMBER, GIT_SHA, BUILD_DATE: as the Dockerfile's build arguments.
#
# Cargo decides what to build again from the files' times, and a fresh
# checkout makes every file new. So every file of the checkout is first
# dated 2000-01-01, then the files that changed since the commit the kept
# build is of (CACHE/target/built-from) are dated now, with their folders
# (a removed file dates its folder). Without that commit (no kept build, or
# one of a commit this clone can't reach) every file is dated now: the
# workspace's crates are all built again, the dependencies still kept.
# Run it from the top of a git clone.
set -euo pipefail

cache="${1:?usage: build-binary.sh CACHE OUT}"
out="${2:?usage: build-binary.sh CACHE OUT}"
image="$(awk 'toupper($1) == "FROM" && $2 ~ /^rust:/ { print $2; exit }' Dockerfile)"
[[ "$image" == *@sha256:* ]] || { echo "::error::no pinned rust image in the Dockerfile"; exit 1; }
mkdir -p "$cache/target" "$cache/home"
cache="$(cd "$cache" && pwd)"

old=946684800 # 2000-01-01T00:00:00Z
built_from=""
if [ -f "$cache/target/built-from" ]; then built_from="$(tr -dc 0-9a-f < "$cache/target/built-from")"; fi
if [ -n "$built_from" ] && ! git cat-file -e "$built_from^{commit}" 2>/dev/null; then
  # a shallow clone: fetch that one commit (only its tree is needed)
  git fetch -q --no-tags --depth=1 origin "$built_from" 2>/dev/null || true
fi
if [ -n "$built_from" ] && git cat-file -e "$built_from^{commit}" 2>/dev/null; then
  git ls-files -z | xargs -0 -r touch -h -d "@$old"
  find . -path ./.git -prune -o -type d -print0 | xargs -0 -r touch -h -d "@$old"
  changed=0
  while IFS= read -r -d '' f; do
    changed=$((changed + 1))
    [ -e "$f" ] && touch -h "$f"
    d="$(dirname "$f")"
    while [ "$d" != . ] && [ ! -d "$d" ]; do d="$(dirname "$d")"; done
    while [ "$d" != . ]; do touch -h "$d"; d="$(dirname "$d")"; done
  done < <(git diff -z --no-renames --name-only "$built_from" HEAD)
  touch -h .
  echo "the kept build is of ${built_from:0:7}: $changed files changed since"
else
  git ls-files -z | xargs -0 -r touch -h
  echo "no kept build of a commit here: the workspace's crates are built again"
fi
rm -f "$cache/target/built-from"

docker run --rm \
  --user "$(id -u):$(id -g)" \
  -v "$PWD:/src:ro" -v "$cache:/cache" -w /src \
  -e CARGO_HOME=/cache/home -e CARGO_TARGET_DIR=/cache/target -e CARGO_TERM_COLOR=always \
  -e SENTRY_MATE_BUILD="${BUILD_NUMBER:-}" -e SENTRY_MATE_COMMIT="${GIT_SHA:-}" -e SENTRY_MATE_DATE="${BUILD_DATE:-}" \
  "$image" \
  sh -c 'apk info -e musl-dev >/dev/null && cargo build --release --locked -p sentry-mate'
git rev-parse HEAD > "$cache/target/built-from"

rm -rf "$out"
mkdir -p "$out/out"
cp "$cache/target/release/sentry-mate" "$out/sentry-mate"
install -d -m 700 "$out/out/data" "$out/out/keys"
"$out/sentry-mate" --version
