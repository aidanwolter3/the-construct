#!/usr/bin/env bash
# Builds the web client (decision `web-client`, docs/clients.md#web) into <out> (default target/web):
# construct_web.js and construct_web_bg.wasm, which the server serves at /web/pkg/ with CONSTRUCT_WEB=1
# (CONSTRUCT_WEB_DIR=<out>). Needs the wasm32-unknown-unknown target and the wasm-bindgen CLI at the version
# in Cargo.lock: rustup target add wasm32-unknown-unknown; cargo install wasm-bindgen-cli --version <it> --locked
#   scripts/web_build.sh [out] [--dev]
set -euo pipefail
cd "$(dirname "$0")/.."
out=${1:-target/web}
profile=release
[ "${2:-}" = "--dev" ] && profile=dev
want=$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | grep version | cut -d'"' -f2)
have=$(wasm-bindgen --version 2>/dev/null | cut -d' ' -f2 || true)
if [ "$want" != "$have" ]; then
  echo "wasm-bindgen CLI $want is needed (have: ${have:-none}): cargo install wasm-bindgen-cli --version $want --locked" >&2
  exit 1
fi
cargo build -q -p construct-web --target wasm32-unknown-unknown --profile "$profile"
dir=$([ "$profile" = dev ] && echo debug || echo release)
mkdir -p "$out"
wasm-bindgen --target web --no-typescript --out-dir "$out" "target/wasm32-unknown-unknown/$dir/construct_web.wasm"
# The page, and the renderer's fonts, which the client fetches instead of carrying.
cp crates/construct-web/static/* "$out/"
mkdir -p "$out/fonts"
cp crates/construct-render/fonts/*.ttf "$out/fonts/"
ls -l "$out"
