FROM rust:alpine AS builder

WORKDIR /opt/src/race-dns-proxy
RUN apk add --no-cache musl-dev cmake make git g++ zstd-dev perl
COPY Cargo.lock Cargo.toml src /opt/src/race-dns-proxy/
COPY src /opt/src/race-dns-proxy/src

ARG CARGO_PROFILE=release
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,sharing=private,target=/opt/src/race-dns-proxy/target \
    cargo build --profile ${CARGO_PROFILE} \
    && cp target/${CARGO_PROFILE}/race-dns-proxy /opt/race-dns-proxy

FROM alpine:latest

WORKDIR /app

COPY --from=builder /opt/race-dns-proxy /app/race-dns-proxy
COPY race-dns-proxy.toml /app/race-dns-proxy.toml

EXPOSE 5653/tcp
EXPOSE 5653/udp
ENTRYPOINT ["/app/race-dns-proxy"]
CMD ["-c", "/app/race-dns-proxy.toml"]
