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
# The workspace's crates are built incremental (CARGO_PROFILE_RELEASE_INCREMENTAL):
# with the kept folder a change to web builds again in well under a minute,
# where a whole build of web and sentry-mate takes over 3 (the dependencies
# are not incremental; the same optimisation level, more codegen units).
# rustc leaves a crate's previous incremental session beside the new one
# (it removes it only when that crate is built the next time), so a kept
# folder carried two of every crate built: 1.1 GB of incremental data became
# about 2 GB in one run, the folder passed 3 GB and the next run (79) started
# over from nothing (8 minutes). Only the newest session of each crate is
# kept now, before the size is checked and after the build.
# A kept folder still past 3 GB (old dependencies after many Cargo.lock
# changes) is dropped and the build starts over.
# The build's --timings report: the slowest units are printed at the end.
# Run it from the top of a git clone.
set -euo pipefail

cache="${1:?usage: build-binary.sh CACHE OUT}"
out="${2:?usage: build-binary.sh CACHE OUT}"
image="$(awk 'toupper($1) == "FROM" && $2 ~ /^rust:/ { print $2; exit }' Dockerfile)"
[[ "$image" == *@sha256:* ]] || { echo "::error::no pinned rust image in the Dockerfile"; exit 1; }

# Each crate's folder in target/release/incremental holds its sessions,
# s-<time>-<id>-<hash>/ with s-<time>-<id>.lock: all but the newest finished
# one (and its lock) are removed.
prune_incremental() {
  local crate newest s
  for crate in "$1"/release/incremental/*/; do
    [ -d "$crate" ] || continue
    newest="$(ls -1dt "$crate"s-*/ 2>/dev/null | grep -v -- '-working/$' | head -1 || true)"
    newest="${newest%/}"
    for s in "$crate"s-*; do
      [ "$s" = "$newest" ] || [ "$s" = "${newest%-*}.lock" ] || rm -rf "$s"
    done
  done
}
if [ -d "$cache/target" ]; then prune_incremental "$cache/target"; fi
if [ -d "$cache/target" ] && [ "$(du -sm "$cache/target" | cut -f1)" -gt 3000 ]; then
  echo "the kept build folder is over 3 GB: built again from nothing"
  rm -rf "$cache/target"
fi
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

# The packs' scripts and style sheets bundled and minified (ui/tools/bundle: esbuild, pinned by its
# package-lock.json), embedded by crates/web/build.rs (SENTRY_MATE_UI_BUNDLE) and served in place of the sources.
# The bundle's files get their pack's newest source time, so a kept build is not built again for it alone.
npm ci --prefix ui/tools/bundle --no-audit --no-fund --loglevel=error
node ui/tools/bundle/bundle.mjs "$cache/ui-bundle"

docker run --rm \
  --user "$(id -u):$(id -g)" \
  -v "$PWD:/src:ro" -v "$cache:/cache" -w /src \
  -e CARGO_HOME=/cache/home -e CARGO_TARGET_DIR=/cache/target -e CARGO_TERM_COLOR=always -e CARGO_PROFILE_RELEASE_INCREMENTAL=true \
  -e SENTRY_MATE_UI_BUNDLE=/cache/ui-bundle \
  -e SENTRY_MATE_BUILD="${BUILD_NUMBER:-}" -e SENTRY_MATE_COMMIT="${GIT_SHA:-}" -e SENTRY_MATE_DATE="${BUILD_DATE:-}" \
  "$image" \
  sh -c 'apk info -e musl-dev >/dev/null && cargo build --release --locked -p sentry-mate --timings'
prune_incremental "$cache/target"
# the slowest units of this build (the report itself is not kept)
python3 - "$cache"/target/cargo-timings/cargo-timing.html <<'PY' || true
import json, re, sys
units = json.loads(re.search(r"const UNIT_DATA = (\[.*?\]);\n", open(sys.argv[1]).read(), re.S).group(1))
units.sort(key=lambda u: -u["duration"])
print("the slowest units: " + ", ".join(f"{u['name']}{' (' + u['target'].strip() + ')' if u.get('target', '').strip() else ''} {u['duration']:.1f} s" for u in units[:8]))
PY
rm -rf "$cache/target/cargo-timings"
du -sh "$cache/target" "$cache/target/release/incremental" "$cache/home" 2>/dev/null || true
git rev-parse HEAD > "$cache/target/built-from"

rm -rf "$out"
mkdir -p "$out/out"
cp "$cache/target/release/sentry-mate" "$out/sentry-mate"
install -d -m 700 "$out/out/data" "$out/out/keys"
"$out/sentry-mate" --version
