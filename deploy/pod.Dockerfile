# The pod (crates/server) as a small runtime image for Fly.io (deploy/fly/pod.toml).
#
#   docker build -f deploy/pod.Dockerfile -t construct-pod .     # from the repo root
#   docker run --rm -p 8080:8080 -v construct-data:/data construct-pod
#
# Runtime config is environment-only (README "Server runtime configuration").

ARG RUST_IMAGE=rust:1-slim-bookworm
# Runs as root on purpose: Fly volumes mount root-owned at /data.
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian12

FROM ${RUST_IMAGE} AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY fixtures ./fixtures
ENV CARGO_PROFILE_RELEASE_STRIP=true \
    CARGO_PROFILE_RELEASE_LTO=thin \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
RUN cargo build --release --locked -p server \
    && cp target/release/server /server

FROM ${RUNTIME_IMAGE}
COPY --from=build /server /server
ENV CONSTRUCT_ADDR=0.0.0.0:8080 \
    CONSTRUCT_DATA_DIR=/data \
    RUST_LOG=info
EXPOSE 8080
ENTRYPOINT ["/server"]
