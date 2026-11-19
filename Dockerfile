# Sentry Mate: one static binary on an empty image, run as a normal user.
#
#   docker build -t ghcr.io/skittles-fivem/sentry-mate:latest .
#
# The same file builds for amd64 and arm64, each on a machine of its kind
# (.github/workflows/image.yml). Data in /data (sealed), the key file in /keys.
# It runs as 1000:1000; TrueNAS runs it as its apps user with user: "568:568".
# The folders must belong to that user.
#
# Both folders must be mounted from the server. There is no VOLUME, and with
# SENTRY_MATE_REQUIRE_MOUNTS=1 each command that uses them first checks that
# each is a mount of its own: a folder left out of the compose file stops it
# with the line to add, instead of being lost with the container (the key
# file with it). The empty /data and /keys below are only where they go.

# The manifests only, with empty sources: the layers that fetch and build
# the dependencies are made again only when a Cargo.toml or Cargo.lock
# changes, not on a code change. (A .cargo/config.toml and a vendor/ folder
# from `cargo vendor`, when there, are kept: a build with no network.)
# The Rust image is pinned by digest (both stages): a moved tag on Docker Hub
# can't change what builds the release. Dependabot (.github/dependabot.yml)
# proposes a new digest; crates/sentry-mate/tests/repo_hygiene.rs checks it.
FROM rust:1.100.0-alpine@sha256:8d0e82abfdf9a38071e07cabc5afba4ddbe077631a80e4e742a760c38beadc33 AS manifests
WORKDIR /src
COPY . .
RUN for f in crates/*/src/lib.rs; do if [ -f "$f" ]; then : > "$f"; fi; done \
 && for f in crates/*/src/main.rs; do if [ -f "$f" ]; then echo 'fn main() {}' > "$f"; fi; done \
 && find . -type f ! -name Cargo.toml ! -name Cargo.lock ! -path './.cargo/*' ! -path './vendor/*' \
      ! -path './crates/*/src/lib.rs' ! -path './crates/*/src/main.rs' -delete \
 && find . -type d -empty -delete

FROM rust:1.100.0-alpine@sha256:8d0e82abfdf9a38071e07cabc5afba4ddbe077631a80e4e742a760c38beadc33 AS build
# (the Rust image has musl-dev and gcc already: nothing to fetch then)
RUN apk info -e musl-dev >/dev/null || apk add --no-cache musl-dev
WORKDIR /src
COPY --from=manifests /src ./
RUN cargo build --release --locked -p sentry-mate
COPY . .
# Set by the release build (.github/workflows/image.yml): the build number
# (the Image workflow's run number), the commit and the date. They show in
# `sentry-mate --version` and the image's labels; a local build has none.
ARG BUILD_NUMBER=""
ARG GIT_SHA=""
ARG BUILD_DATE=""
# (touched: the copied sources are older than the empty ones built above)
RUN find crates -name '*.rs' -exec touch {} + \
 && SENTRY_MATE_BUILD="$BUILD_NUMBER" SENTRY_MATE_COMMIT="$GIT_SHA" SENTRY_MATE_DATE="$BUILD_DATE" \
    cargo build --release --locked -p sentry-mate \
 && ./target/release/sentry-mate --version \
 && mkdir -p /out/data /out/keys \
 && chmod 700 /out/data /out/keys

FROM scratch
COPY --from=build /src/target/release/sentry-mate /sentry-mate
# (/out/ whole, not each folder: COPY of a folder copies only what is in it,
# and the folder it makes for that is mode 755)
COPY --from=build --chown=1000:1000 /out/ /
USER 1000:1000
# (/data and /keys are the defaults; not set here, or every usage message
# would show --data and --keys as if they had to be given)
ENV SENTRY_MATE_REQUIRE_MOUNTS=1
# CAN Bridge uploads and the web UI, both HTTPS (TLS 1.3 only), one certificate
EXPOSE 10100 9100
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --start-interval=2s CMD ["/sentry-mate", "health"]
ARG BUILD_NUMBER=""
ARG GIT_SHA=""
ARG BUILD_DATE=""
LABEL org.opencontainers.image.title="Sentry Mate" \
      org.opencontainers.image.description="A self-hosted car logger fed by the Screenmate CAN Bridge's uploads" \
      org.opencontainers.image.source="https://github.com/skittles-fivem/Sentry-Mate" \
      org.opencontainers.image.revision="$GIT_SHA" \
      org.opencontainers.image.created="$BUILD_DATE" \
      org.opencontainers.image.version="${BUILD_NUMBER:+build-$BUILD_NUMBER}"
ENTRYPOINT ["/sentry-mate"]
CMD ["serve"]
