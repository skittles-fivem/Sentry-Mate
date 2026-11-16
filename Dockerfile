# Sentry Mate: one static binary on an empty image, run as a normal user.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p sentry-mate \
 && mkdir -p /out/data \
 && chown 1000:1000 /out/data

FROM scratch
COPY --from=build /src/target/release/sentry-mate /sentry-mate
COPY --from=build --chown=1000:1000 /out/data /data
USER 1000:1000
ENV SENTRY_MATE_DATA=/data
EXPOSE 10100
VOLUME ["/data"]
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s CMD ["/sentry-mate", "health"]
ENTRYPOINT ["/sentry-mate"]
CMD ["serve"]
