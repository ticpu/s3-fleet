# Cross-compiles both release binaries in one pass: x86_64 and aarch64 musl,
# static-pie through rust-lld, with no glibc floor. TLS (reqwest's rustls on
# aws-lc-rs) brings C code, built with clang against each target's musl
# headers from Debian multiarch; the readelf check at the end proves no
# shared library slipped in.
FROM docker.io/library/debian:bookworm AS build

RUN --mount=type=cache,target=/var/cache/apt,id=apt-cache-bookworm \
    --mount=type=cache,target=/var/lib/apt/lists,id=apt-lists-bookworm \
    dpkg --add-architecture arm64 \
    && apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    build-essential \
    clang \
    cmake \
    pkg-config \
    musl-dev \
    musl-dev:arm64

ENV CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup
ENV PATH="/usr/local/cargo/bin:${PATH}"

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path

RUN rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl

ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static -C linker=rust-lld" \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static -C linker=rust-lld" \
    CC_x86_64_unknown_linux_musl=clang \
    CC_aarch64_unknown_linux_musl=clang \
    CFLAGS_x86_64_unknown_linux_musl="--target=x86_64-linux-musl -isystem /usr/include/x86_64-linux-musl" \
    CFLAGS_aarch64_unknown_linux_musl="--target=aarch64-linux-musl -isystem /usr/include/aarch64-linux-musl"

WORKDIR /app

# Cargo.lock is only committed on release commits, hence the glob
COPY Cargo.toml Cargo.lock* ./
COPY src ./src

RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry \
    --mount=type=cache,target=/usr/local/cargo/git,id=cargo-git \
    --mount=type=cache,target=/app/target,id=cargo-target-s3-fleet \
    set -eu; \
    for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do \
        cargo build --release --target "$target" --bin s3-fleet; \
    done; \
    mkdir /out; \
    cp target/x86_64-unknown-linux-musl/release/s3-fleet /out/s3-fleet.amd64; \
    cp target/aarch64-unknown-linux-musl/release/s3-fleet /out/s3-fleet.arm64; \
    for arch in amd64 arm64; do \
        if readelf -d "/out/s3-fleet.$arch" | grep -q NEEDED; then \
            echo "ERROR: s3-fleet.$arch links a shared library" >&2; \
            readelf -d "/out/s3-fleet.$arch" | grep NEEDED >&2; \
            exit 1; \
        fi; \
    done

FROM scratch
COPY --from=build /out/ /
