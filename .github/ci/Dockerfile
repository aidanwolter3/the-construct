# The image CI's Rust job runs in (.github/workflows/ci.yml, decision `ci-container-image`): everything the job
# needs, installed once, so a run downloads nothing from Linux package mirrors. CI's `image` job builds it from a
# context holding only rust-toolchain.toml, tags it with a hash of this file and that one, and pushes it to
# ghcr.io only when that tag is new: change either file and the next run builds the new image.
#
# Debian, not Ubuntu: the GitHub runners' Ubuntu mirrors are what failed (issue #246), and the base is pinned by
# digest. Runs as uid 1001 (the runner's user), so tests that need a non-root user (read-only files) still run.

FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

LABEL org.opencontainers.image.source=https://github.com/aidanwolter3/the-construct
LABEL org.opencontainers.image.description="CI image for The Construct's Rust job"

# The same packages scripts/cloud_setup.sh installs: a C toolchain, the simulator's fonts, lavapipe (Mesa's
# software Vulkan, which construct-render's GPU tests draw on), plus git, zstd and xz for checkout and the cache.
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl git jq sqlite3 pkg-config \
        fonts-dejavu-core fontconfig \
        mesa-vulkan-drivers libvulkan1 \
        xz-utils zstd \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

# The pinned Rust (rust-toolchain.toml), as the default, and cargo-nextest.
ARG NEXTEST_VERSION=0.9.145
COPY rust-toolchain.toml /tmp/toolchain/
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path --profile minimal --default-toolchain none \
    && cd /tmp/toolchain \
    && rustup toolchain install \
    && rustup default "$(rustup show active-toolchain | cut -d' ' -f1)" \
    && curl --proto '=https' --tlsv1.2 -LsSf "https://get.nexte.st/${NEXTEST_VERSION}/linux" \
        | tar zxf - -C "$CARGO_HOME/bin" \
    && rm -rf /tmp/toolchain

RUN useradd --uid 1001 --create-home runner && chown -R runner "$CARGO_HOME" "$RUSTUP_HOME"
