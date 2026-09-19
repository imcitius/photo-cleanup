# Two ways out of this file:
#
#   1. a statically linked binary you copy to the NAS and run directly —
#      no container, no glibc version to match:
#        docker build --target export --output type=local,dest=./dist .
#
#   2. a runnable image, if you would rather keep it in Docker:
#        docker build -t photo-cleanup:dev .
#
# Build this ON the NAS: its host architecture is amd64, so the compiler runs
# natively. Building it on an arm64 Mac forces QEMU emulation and takes an
# order of magnitude longer.

FROM rust:1.98-slim-bookworm AS builder
ARG TARGET=x86_64-unknown-linux-musl
RUN apt-get update \
 && apt-get install -y --no-install-recommends musl-tools \
 && rm -rf /var/lib/apt/lists/* \
 && rustup target add "$TARGET"
# rusqlite compiles SQLite from source, so its C compiler has to target musl.
# The *linker* is deliberately left alone: Rust links musl targets statically
# against its own bundled libc, whereas musl-gcc would link against
# /lib/ld-musl-x86_64.so.1 — a loader that does not exist on a glibc host such
# as Unraid, and the binary would fail to start with a bare "not found".
ENV CC_x86_64_unknown_linux_musl=musl-gcc
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build --release --locked --target "$TARGET" -p pc-cli \
 && cp "target/$TARGET/release/photo-cleanup" /photo-cleanup \
 && strip /photo-cleanup
# Fail the build rather than ship something that cannot start on the target.
# A PT_INTERP segment means the binary wants a dynamic loader.
RUN if readelf -l /photo-cleanup | grep -q INTERP; then \
        echo "БИНАРЬ НЕ СТАТИЧЕСКИЙ — на glibc-хосте не запустится:" >&2; \
        readelf -l /photo-cleanup | grep -A2 INTERP >&2; \
        exit 1; \
    fi \
 && /photo-cleanup --version

# `--output type=local` writes just this layer to the host.
FROM scratch AS export
COPY --from=builder /photo-cleanup /photo-cleanup

# A static binary needs nothing around it.
FROM scratch AS runtime
COPY --from=builder /photo-cleanup /photo-cleanup
WORKDIR /data
ENTRYPOINT ["/photo-cleanup"]
CMD ["--help"]
