# The image CI's Rust job runs in (.github/workflows/ci.yml, decision `ci-container-image`): everything the job
# needs, installed once, so a run downloads nothing from Linux package mirrors. CI's `image` job builds it from a
# context holding only rust-toolchain.toml and tags it with a hash of this file and that one. Only main's runs push
# it (tag `main-<hash>`), and main's Rust job runs it by the digest main recorded; a pull request that changes either
# file builds it without pushing, and its tests run in main's image.
#
# Debian, not Ubuntu: the GitHub runners' Ubuntu mirrors are what failed (issue #246), and the base is pinned by
# digest. Runs as uid 1001 (the runner's user), so tests that need a non-root user (read-only files) still run.

FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

LABEL org.opencontainers.image.source=https://github.com/aidanwolter3/the-construct
LABEL org.opencontainers.image.description="CI image for The Construct's Rust job"

# The same packages scripts/cloud_setup.sh installs: a C toolchain, the simulator's fonts, lavapipe (Mesa's
# software Vulkan, which construct-render's GPU tests draw on), plus git, zstd and xz for checkout and the cache,
# procps (`kill`: the server's shutdown tests signal it), and chromium (web-extract's render tests; they skip
# without it).
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl git jq sqlite3 pkg-config \
        fonts-dejavu-core fontconfig \
        mesa-vulkan-drivers libvulkan1 \
        procps xz-utils zstd \
        chromium \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

# The pinned Rust (rust-toolchain.toml), as the default, and cargo-nextest. rustup-init and the nextest tarball are
# downloaded at pinned versions and checked against pinned sha256s (issue #441); rustup then checks the toolchain's
# own downloads. To bump one: change its version and its hash (rustup publishes `rustup-init.sha256` next to the
# binary; for nextest, `sha256sum` the release's x86_64-unknown-linux-gnu tarball).
ARG RUSTUP_VERSION=1.29.3
ARG RUSTUP_SHA256=b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55
ARG NEXTEST_VERSION=0.9.145
ARG NEXTEST_SHA256=32aa82416099eb12fffae9cf1a279ad201fecbd3f74826c613e32e9006b29867
COPY rust-toolchain.toml /tmp/toolchain/
RUN cd /tmp/toolchain \
    && curl --proto '=https' --tlsv1.2 -fsSLo rustup-init \
        "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/x86_64-unknown-linux-gnu/rustup-init" \
    && echo "${RUSTUP_SHA256}  rustup-init" | sha256sum -c - \
    && chmod +x rustup-init \
    && ./rustup-init -y --no-modify-path --profile minimal --default-toolchain none \
    && rustup toolchain install \
    && rustup default "$(rustup show active-toolchain | cut -d' ' -f1)" \
    && curl --proto '=https' --tlsv1.2 -fsSLo nextest.tar.gz \
        "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-${NEXTEST_VERSION}/cargo-nextest-${NEXTEST_VERSION}-x86_64-unknown-linux-gnu.tar.gz" \
    && echo "${NEXTEST_SHA256}  nextest.tar.gz" | sha256sum -c - \
    && tar zxf nextest.tar.gz -C "$CARGO_HOME/bin" \
    && cd / && rm -rf /tmp/toolchain

RUN useradd --uid 1001 --create-home runner && chown -R runner "$CARGO_HOME" "$RUSTUP_HOME"
