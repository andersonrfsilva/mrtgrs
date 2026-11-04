FROM rust:1-alpine AS builder
RUN apk add --no-cache musl-dev pkgconf rrdtool-dev
WORKDIR /build
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
# musl links statically by default and pulls librrd.a without its glib deps; link librrd.so instead.
RUN RUSTFLAGS="-C target-feature=-crt-static" cargo build --release --locked

FROM alpine:3.21
RUN apk add --no-cache libgcc rrdtool tzdata && addgroup -S appgroup && adduser -S appuser -G appgroup
COPY --from=builder /build/target/release/mrtg-rs /usr/local/bin/mrtg-rs
COPY www/ /app/www/
USER appuser
EXPOSE 8088
HEALTHCHECK --interval=60s --timeout=5s --start-period=5s --retries=3 CMD ["/usr/local/bin/mrtg-rs", "--healthcheck"]
ENTRYPOINT ["/usr/local/bin/mrtg-rs"]
