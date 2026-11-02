#!/usr/bin/env bash
# Cross-compile the client-ffi cdylib for Android with cargo-ndk.
#
# Usage: scripts/build_android.sh
#
# Environment (all optional):
#   ANDROID_NDK_HOME     NDK to use. Default: newest NDK under <sdk>/ndk, where <sdk> is
#                        $ANDROID_HOME if set, else $ANDROID_SDK_ROOT if set, else
#                        ~/Library/Android/sdk (the first one set is used; no fallback).
#   CONSTRUCT_ANDROID_ABIS  Space- or comma-separated ABIs. Default: "arm64-v8a x86_64".
#
# Outputs:
#   kmp-app/androidApp/src/main/jniLibs/<abi>/libclient_ffi.so
# Prerequisites: cargo install cargo-ndk;
#   rustup target add aarch64-linux-android x86_64-linux-android
# Idempotent; safe to re-run.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

OUT_DIR="kmp-app/androidApp/src/main/jniLibs"
ABIS="${CONSTRUCT_ANDROID_ABIS:-arm64-v8a x86_64}"
ABIS="${ABIS//,/ }"

if ! cargo ndk --version >/dev/null 2>&1; then
    echo "error: cargo-ndk not found. Install it with: cargo install cargo-ndk" >&2
    exit 1
fi

if [[ -z "${ANDROID_NDK_HOME:-}" ]]; then
    SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}"
    if [[ -d "$SDK/ndk" ]]; then
        # Newest complete NDK (has source.properties; skips e.g. "*.bak" leftovers).
        NEWEST=""
        for dir in "$SDK"/ndk/*/; do
            dir="${dir%/}"
            [[ -f "$dir/source.properties" ]] && NEWEST+="$(basename "$dir")"$'\n'
        done
        NEWEST="$(printf '%s' "$NEWEST" | sort -V | tail -n 1)"
        if [[ -n "$NEWEST" ]]; then
            ANDROID_NDK_HOME="$SDK/ndk/$NEWEST"
        fi
    fi
fi
if [[ -z "${ANDROID_NDK_HOME:-}" || ! -d "$ANDROID_NDK_HOME" ]]; then
    echo "error: no Android NDK found. Set ANDROID_NDK_HOME, or install one with the" >&2
    echo "       Android Studio SDK Manager (into \$ANDROID_HOME/ndk/<version>)." >&2
    exit 1
fi
export ANDROID_NDK_HOME

TARGET_ARGS=()
for abi in $ABIS; do
    case "$abi" in
        arm64-v8a|armeabi-v7a|x86_64|x86) TARGET_ARGS+=(-t "$abi") ;;
        *) echo "error: unknown ABI '$abi' in CONSTRUCT_ANDROID_ABIS" >&2; exit 1 ;;
    esac
done
if [[ ${#TARGET_ARGS[@]} -eq 0 ]]; then
    echo "error: CONSTRUCT_ANDROID_ABIS is empty" >&2
    exit 1
fi

echo "==> NDK: $ANDROID_NDK_HOME"
echo "==> Building client-ffi (release) for: $ABIS"
mkdir -p "$OUT_DIR"
cargo ndk "${TARGET_ARGS[@]}" -o "$OUT_DIR" build -p client-ffi --release --lib

for abi in $ABIS; do
    SO="$OUT_DIR/$abi/libclient_ffi.so"
    if [[ ! -f "$SO" ]]; then
        echo "error: expected $SO" >&2
        exit 1
    fi
    echo "==> $SO ($(du -h "$SO" | cut -f1))"
done
echo "==> Done"
