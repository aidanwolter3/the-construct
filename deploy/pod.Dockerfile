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
# Runs as root on purpose: Fly volumes mount root-owned at /data.
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian12@sha256:e5d81ddde149641e2a9ba55be4545bc125c67de07508b03ba4c22e6eb0ded5aa

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
# Layout files (layouts as code) are embedded in the binary at compile time.
COPY layouts ./layouts
RUN cargo build --release --locked -p server \
    && cp target/release/server /server

FROM ${RUNTIME_IMAGE}
COPY --from=build /server /server
ENV CONSTRUCT_ADDR=0.0.0.0:8080 \
    CONSTRUCT_DATA_DIR=/data \
    RUST_LOG=info
EXPOSE 8080
ENTRYPOINT ["/server"]
