# Three ways out of this file:
#
#   1. a statically linked binary you copy anywhere and run directly —
#      no container, no glibc version to match:
#        docker build --target export --output type=local,dest=./dist .
#
#   2. a runnable image:
#        docker build -t photo-cleanup:dev .
#
#   3. what CI publishes: the same image for linux/amd64 and linux/arm64,
#      built on a runner of each architecture so nothing is emulated.
#
# The build is native by design. Cross-compiling would mean a musl C
# toolchain for the other architecture, because rusqlite compiles SQLite
# from source, and that is a great deal of machinery to keep working for no
# gain over a second runner.

FROM rust:1.98-slim-bookworm AS builder
# Set by buildx; equal to the host architecture on a native build.
ARG TARGETARCH
RUN set -eux; \
    case "${TARGETARCH:-amd64}" in \
      amd64) target=x86_64-unknown-linux-musl ;; \
      arm64) target=aarch64-unknown-linux-musl ;; \
      *) echo "unknown architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    echo "$target" > /target; \
    apt-get update; \
    apt-get install -y --no-install-recommends musl-tools; \
    rm -rf /var/lib/apt/lists/*; \
    rustup target add "$target"
# rusqlite compiles SQLite from source, so its C compiler has to target musl.
# The *linker* is deliberately left alone: Rust links musl targets statically
# against its own bundled libc, whereas musl-gcc would link against
# /lib/ld-musl-*.so.1 — a loader that does not exist on a glibc host such as
# Unraid, and the binary would fail to start with a bare "not found".
# Only the variable matching the chosen target is ever read.
ENV CC_x86_64_unknown_linux_musl=musl-gcc \
    CC_aarch64_unknown_linux_musl=musl-gcc
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN target="$(cat /target)" \
 && cargo build --release --locked --target "$target" -p pc-cli \
 && cp "target/$target/release/photo-cleanup" /photo-cleanup \
 && strip /photo-cleanup
# Fail the build rather than ship something that cannot start on the target.
# A PT_INTERP segment means the binary wants a dynamic loader.
RUN if readelf -l /photo-cleanup | grep -q INTERP; then \
        echo "THE BINARY IS NOT STATIC — it will not run on a glibc host:" >&2; \
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
# The database and the thumbnail cache live here; mount a volume over it.
WORKDIR /data
# A scratch image has no PATH at all, so `docker exec <container>
# photo-cleanup inspect ...` could not find the very binary the container is
# running. The root is the only directory there is; now the name resolves.
ENV PATH="/"
EXPOSE 8080
ENTRYPOINT ["/photo-cleanup"]
# Serving on every interface is the point of running this in a container: the
# archive is on the NAS and the browser is not.
CMD ["--db", "/data/photo-cleanup.db", "serve", "--thumbs", "/data/thumbs", "--bind", "0.0.0.0:8080"]
