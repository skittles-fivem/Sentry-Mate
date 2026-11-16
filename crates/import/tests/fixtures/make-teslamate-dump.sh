#!/usr/bin/env bash
# Makes a TeslaMate backup fixture for the import tests, the real way: the official TeslaMate image creates its own
# schema in a throwaway Postgres, fake rows go in, and the backup is made with TeslaMate's documented command
# (pg_dump of the teslamate database, plain SQL).
#
#   make-teslamate-dump.sh <teslamate image> <out.sql> <fake rows.sql>... [-- <later rows.sql>... <later out.sql>]
#
# e.g. make-teslamate-dump.sh teslamate/teslamate:v4.4.0 teslamate-4.4.0.sql teslamate-fake-rows.sql \
#        -- teslamate-fake-rows-later.sql teslamate-4.4.0-later.sql
#
# What is kept: pg_dump's own lines (header, SET, setval, the closing "dump complete" line) and every COPY block,
# exactly as pg_dump wrote them, in its order. TeslaMate's schema statements (tables, indexes, its SQL functions) are
# taken out: they are TeslaMate's code, which this repository doesn't copy, and the importer never reads them. Set
# KEEP_FULL=<path prefix> to also keep the whole backups (for checking locally, never committed); then
# TESLAMATE_FULL_DUMPS=<the same prefix> cargo test -p import checks they import exactly like the fixtures.
# POSTGRES_IMAGE picks the Postgres image (default postgres:16; e.g. mirror.gcr.io/library/postgres:16).
#
# Needs Docker. Everything it starts is removed at the end.
set -euo pipefail

image=$1
out=$2
shift 2
first=()
while [ $# -gt 0 ] && [ "$1" != "--" ]; do first+=("$1"); shift; done
later=()
later_out=
if [ "${1:-}" = "--" ]; then
  shift
  while [ $# -gt 1 ]; do later+=("$1"); shift; done
  later_out=$1
fi

postgres=${POSTGRES_IMAGE:-postgres:16}
net=tm-fixture-$$
db=tm-fixture-db-$$
app=tm-fixture-app-$$
cleanup() { docker rm -f "$app" "$db" > /dev/null 2>&1 || true; docker network rm "$net" > /dev/null 2>&1 || true; }
trap cleanup EXIT

docker network create "$net" > /dev/null
docker run -d --name "$db" --network "$net" -e POSTGRES_USER=teslamate -e POSTGRES_PASSWORD=fixture \
  -e POSTGRES_DB=teslamate "$postgres" > /dev/null
until docker exec "$db" pg_isready -U teslamate -d teslamate > /dev/null 2>&1; do sleep 1; done
sleep 2

# TeslaMate migrates the database when it starts (it never signs in to Tesla here: there is no token).
docker run -d --name "$app" --network "$net" -e ENCRYPTION_KEY=fixture-only -e DATABASE_USER=teslamate \
  -e DATABASE_PASS=fixture -e DATABASE_NAME=teslamate -e DATABASE_HOST="$db" -e DISABLE_MQTT=true -e TZ=UTC \
  "$image" > /dev/null

psql() { docker exec -i "$db" psql -v ON_ERROR_STOP=1 -q -U teslamate -d teslamate "$@"; }
migrations() { psql -At -c 'SELECT count(*) FROM schema_migrations' 2> /dev/null || echo 0; }
last=-1
stable=0
for _ in $(seq 1 180); do
  n=$(migrations)
  if [ "$n" != "0" ] && [ "$n" = "$last" ]; then stable=$((stable + 1)); else stable=0; fi
  [ "$stable" -ge 5 ] && break
  last=$n
  sleep 1
done
[ "$stable" -ge 5 ] || { echo "TeslaMate didn't finish its migrations" >&2; docker logs "$app" | tail -20 >&2; exit 1; }
docker rm -f "$app" > /dev/null

strip() {
  awk '
    copy { print; if ($0 == "\\.") copy = 0; next }
    /^COPY .* FROM stdin;$/ { print; copy = 1; next }
    /^SET / || /^SELECT pg_catalog\./ || /^\\(un)?restrict / { print; next }
    /^-- (PostgreSQL database dump|Dumped (from|by) |Data for Name: )/ { print; next }
  '
}

backup() {
  local into=$1
  # TeslaMate's documented backup: docker compose exec -T database pg_dump -U teslamate teslamate > teslamate.bck
  docker exec "$db" pg_dump -U teslamate teslamate > "$into.full"
  strip < "$into.full" > "$into"
  if [ -n "${KEEP_FULL:-}" ]; then mv "$into.full" "${KEEP_FULL}${into##*/}"; else rm -f "$into.full"; fi
}

for f in "${first[@]}"; do psql < "$f" > /dev/null; done
backup "$out"
if [ -n "$later_out" ]; then
  for f in "${later[@]}"; do psql < "$f" > /dev/null; done
  backup "$later_out"
fi
