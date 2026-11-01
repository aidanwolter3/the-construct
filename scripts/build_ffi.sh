#!/usr/bin/env bash
# Build the client-ffi cdylib (macOS) and generate the UniFFI Kotlin bindings shared by the
# desktop and Android targets (kmp-app :shared, jvmCommonMain source set).
#
# Usage: scripts/build_ffi.sh
#
# Outputs:
#   target/release/libclient_ffi.dylib   (desktop app: -Djna.library.path=<repo>/target/release)
#   kmp-app/shared/src/jvmCommonMain/kotlin/uniffi/client_ffi/client_ffi.kt
# Idempotent; safe to re-run.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

TARGET="aarch64-apple-darwin"
LIB_NAME="libclient_ffi.dylib"
BUILT_LIB="target/$TARGET/release/$LIB_NAME"
OUT_DIR="kmp-app/shared/src/jvmCommonMain/kotlin"

echo "==> Building client-ffi ($TARGET, release)"
cargo build -p client-ffi --release --lib --target "$TARGET"

echo "==> Generating Kotlin bindings into $OUT_DIR"
rm -rf "$OUT_DIR/uniffi/client_ffi"
# The bindgen CLI is behind client-ffi's opt-in `bindgen` feature (keeps uniffi/cli out of
# Android cross-builds).
cargo run -p client-ffi --release --features bindgen --bin uniffi-bindgen -- \
    generate --library "$BUILT_LIB" --language kotlin --out-dir "$OUT_DIR" --no-format

GENERATED="$OUT_DIR/uniffi/client_ffi/client_ffi.kt"
if [[ ! -f "$GENERATED" ]]; then
    echo "error: expected bindings at $GENERATED" >&2
    exit 1
fi
# Copy last: the host `cargo run --release` above may also write a cdylib here.
echo "==> Copying $BUILT_LIB -> target/release/$LIB_NAME"
mkdir -p target/release
cp -f "$BUILT_LIB" "target/release/$LIB_NAME"

echo "==> Done: $GENERATED"
