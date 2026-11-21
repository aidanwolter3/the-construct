# The pod (crates/server: pod-core + the app adapters) as a small runtime image for Fly.io (deploy/fly/pod.toml).
#
#   docker build -f deploy/pod.Dockerfile -t construct-pod .     # from the repo root
#   docker run --rm -p 8080:8080 -v construct-data:/data construct-pod
#
# Runtime config is environment-only (docs/server.md "Configuration").
#
# Dependencies are compiled in their own layer (cargo-chef): it is rebuilt only when Cargo.toml / Cargo.lock
# change, so a code-only deploy recompiles just the workspace crates.

# Base images are pinned by digest (reproducible builds); bump the digest to take updates.
ARG RUST_IMAGE=rust:1-slim-bookworm@sha256:24707e70a4f1e23d9a7b25202ec43126d28a52294b96aad5c670df1fd1f1d400
# Runs as root on purpose: Fly volumes mount root-owned at /data. Debian slim (the CI image's base, decision
# `ci-container-image`) rather than distroless: it carries Chromium, the headless browser of rendered fillers
# (decision `rendered-fetches`). A bookworm-built binary runs on trixie's newer glibc.
ARG RUNTIME_IMAGE=debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

FROM ${RUST_IMAGE} AS chef
RUN cargo install cargo-chef --locked --version 0.1.78
WORKDIR /src
ENV CARGO_PROFILE_RELEASE_STRIP=true \
    CARGO_PROFILE_RELEASE_LTO=thin \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1

# The dependency "recipe": manifests only (changes when Cargo.toml / Cargo.lock do).
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# A workspace member (the server's tests use it), so cargo needs its manifest.
COPY fake-pixelfed ./fake-pixelfed
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked -p server --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY fake-pixelfed ./fake-pixelfed
# The build id the server and its web client report (decision `device-versions`): CI passes
# `--build-arg CONSTRUCT_BUILD=$(scripts/build_id.sh)` (the image has no .git); without it they say `+dev`.
ARG CONSTRUCT_BUILD=
RUN cargo build --release --locked -p server \
    && cp target/release/server /server

# The web client (decision `web-client`): construct-web for wasm32 and its JavaScript glue, with the wasm-bindgen
# CLI at Cargo.lock's version, as scripts/web_build.sh builds it (CI's web job runs the same); served at /web from
# CONSTRUCT_WEB_DIR. Its dependencies are compiled with it (a minute or two of the build), after the server's.
FROM chef AS web
RUN rustup target add wasm32-unknown-unknown
COPY Cargo.lock ./
RUN v=$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | grep version | cut -d'"' -f2) \
    && cargo install wasm-bindgen-cli --locked --version "$v"
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY fake-pixelfed ./fake-pixelfed
COPY assets/icon ./assets/icon
COPY scripts/web_build.sh ./scripts/web_build.sh
# Waits for the server's build, so the two release builds (each with LTO) don't run at once on the builder:
# together they ran out of memory there (rustc killed). The setup above still runs alongside the server's build.
COPY --from=build /server /tmp/server-built-first
ARG CONSTRUCT_BUILD=
RUN scripts/web_build.sh /web

FROM ${RUNTIME_IMAGE}
# Chromium (and fonts, so pages lay out as they do for people) for rendered fillers; CA certificates for it;
# nftables, with which the server fences the browser's user's network at startup (decision
# `browser-network-fence`; without it the server logs a warning and serves anyway).
# Chromium loads untrusted pages, so it must stay current (decision `browser-sandbox`): a builder reuses this
# layer until its inputs change, and CHROMIUM_REFRESH is one. CI's deploy sets it to the ISO week
# (ci.yml, which also deploys weekly), so this layer is rebuilt at least once a week with Debian's latest
# Chromium and security updates (the `upgrade`, for a base digest older than them). By hand, pass
# `--build-arg CHROMIUM_REFRESH=$(date -u +%G-W%V)` too.
ARG CHROMIUM_REFRESH=unset
RUN echo "packages as of ${CHROMIUM_REFRESH}" \
    && apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get upgrade -y \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        chromium ca-certificates fonts-liberation nftables \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /server /server
COPY --from=web /web /web
ENV CONSTRUCT_ADDR=0.0.0.0:8080 \
    CONSTRUCT_WEB_DIR=/web \
    CONSTRUCT_DATA_DIR=/data \
    CONSTRUCT_BROWSER=/usr/bin/chromium \
    RUST_LOG=info
EXPOSE 8080
ENTRYPOINT ["/server"]
