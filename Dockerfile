# Build for the NAS (linux/amd64) from any host:
#   docker buildx build --platform linux/amd64 -t photo-cleanup:dev --load .
FROM rust:1.98-slim-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build --release --locked -p pc-cli

FROM debian:bookworm-slim
RUN useradd -r -u 1000 -m pc
COPY --from=builder /src/target/release/photo-cleanup /usr/local/bin/photo-cleanup
USER pc
WORKDIR /data
ENTRYPOINT ["photo-cleanup"]
CMD ["--help"]
