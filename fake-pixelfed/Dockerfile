# Fake Pixelfed upstream as a tiny sidecar image (docs/testing.md "Fake Pixelfed").
#
#   docker build -f fake-pixelfed/Dockerfile -t construct-fake-pixelfed .   # from the repo root
#   docker run --rm -p 8081:8081 -e FAKE_PIXELFED_ADMIN=1 \
#     -e FAKE_PIXELFED_PUBLIC_URL=http://localhost:8081 construct-fake-pixelfed
#
# The fixtures are compiled into the binary, so the runtime image holds one file.
# The admin/fault endpoints are off by default inside the container (it binds 0.0.0.0,
# not loopback); FAKE_PIXELFED_ADMIN=1 turns them on.

# Base images are pinned by digest (reproducible builds); bump the digest to take updates.
ARG RUST_IMAGE=rust:1-slim-bookworm@sha256:24707e70a4f1e23d9a7b25202ec43126d28a52294b96aad5c670df1fd1f1d400
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

# Dependencies are compiled in their own layer (cargo-chef), rebuilt only when Cargo.toml / Cargo.lock change.
FROM ${RUST_IMAGE} AS chef
RUN cargo install cargo-chef --locked --version 0.1.78
WORKDIR /src
ENV CARGO_PROFILE_RELEASE_STRIP=true \
    CARGO_PROFILE_RELEASE_LTO=thin \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1

# The whole workspace manifest set is needed to resolve the workspace; only
# fake-pixelfed (and its dependencies) is compiled.
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY fake-pixelfed ./fake-pixelfed
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked -p fake-pixelfed --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# The crate and the fixtures it embeds.
COPY fake-pixelfed ./fake-pixelfed
RUN cargo build --release --locked -p fake-pixelfed \
    && cp target/release/fake-pixelfed /fake-pixelfed

FROM ${RUNTIME_IMAGE}
COPY --from=build /fake-pixelfed /fake-pixelfed
ENV FAKE_PIXELFED_ADDR=0.0.0.0:8081
EXPOSE 8081
ENTRYPOINT ["/fake-pixelfed"]
