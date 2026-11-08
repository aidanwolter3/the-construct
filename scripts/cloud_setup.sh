#!/usr/bin/env bash
# Setup for Claude Code cloud environments (Ubuntu containers): the toolchains for the cargo tests and the headless
# simulator, plus warm caches so a session doesn't download and compile hundreds of crates on its first build.
# Use it as the environment's setup script (paste the snippet from CLAUDE.md "Cloud sessions" into the
# environment settings), or run it once in a session: `bash scripts/cloud_setup.sh`. Idempotent.
#
# Cache warm-up: fetches every crate, builds the headless simulator (construct-sim) and installs cargo-nextest. A
# failed step is retried with long, growing waits (30s..4min), never in a tight loop; a warm-up that still fails
# only leaves a warning: sessions then download on first build.
#
# The warm-up runs in the background by default, so the session starts right away. A cargo build started meanwhile
# waits on cargo's lock instead of building twice. Progress goes to ~/.construct-cloud-warmup.log;
# ~/.construct-cloud-warmup.status reads "running <pid>", "done" or "failed <step>". `scripts/await_warmup.sh` waits
# for it, and restarts it when the status says running but the process is gone (a container restart or a killed
# shell leaves that behind).
# Env: CONSTRUCT_SETUP_WARM=background (default) | foreground | skip.
#
# Disk: a session has a fixed allowance (~30 GB). The script turns off incremental compilation for this user
# (~/.cargo/config.toml), which otherwise grows target/debug by many GB per session.
#
# Network access the environment needs (allow these domains in the environment's network policy):
#   static.rust-lang.org, sh.rustup.rs, crates.io, index.crates.io, static.crates.io   (Rust)
#   github.com, api.github.com, objects.githubusercontent.com                          (git, gh)
#   the-construct-pod.fly.dev, the-construct-fake-pixelfed.fly.dev                     (staging checks)
# Not installed: an Android SDK and NDK (the APK is built by CI and on the developer's Mac), macOS tools.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SUDO=""
if [[ $EUID -ne 0 ]] && command -v sudo >/dev/null; then SUDO="sudo"; fi

if command -v apt-get >/dev/null; then
  need=()
  command -v cc >/dev/null || need+=(build-essential)
  command -v jq >/dev/null || need+=(jq)
  command -v sqlite3 >/dev/null || need+=(sqlite3)
  dpkg -s fonts-dejavu-core >/dev/null 2>&1 || need+=(fonts-dejavu-core fontconfig)  # simulator text rendering
  dpkg -s pkg-config >/dev/null 2>&1 || need+=(pkg-config)
  dpkg -s mesa-vulkan-drivers >/dev/null 2>&1 || need+=(mesa-vulkan-drivers)  # lavapipe: construct-render's GPU tests
  if [[ ${#need[@]} -gt 0 ]]; then
    $SUDO apt-get update -y
    DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y "${need[@]}"
  fi
fi

if ! command -v cargo >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --component clippy,rustfmt
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi
# rust-toolchain.toml pins the version CI uses; install it now rather than on the first build.
(cd "$REPO_ROOT" && rustup show active-toolchain >/dev/null 2>&1 || rustup toolchain install) || true

# No incremental compilation in cloud sessions: it is the largest part of target/debug, and a fresh container
# gains nothing from it. Written to the user's cargo config, so the developer's Mac is unaffected.
mkdir -p "$HOME/.cargo"
if ! grep -qs '^incremental' "$HOME/.cargo/config.toml"; then
  printf '\n[build]\nincremental = false\n' >>"$HOME/.cargo/config.toml"
fi

# Runs a command; after a failure waits 30s, 60s, 120s, 240s before the next try (5 tries in all).
with_backoff() {
  local delay=30 attempt=1 max=5
  until "$@"; do
    if ((attempt >= max)); then
      echo "cloud_setup: giving up after $attempt attempts: $*" >&2
      return 1
    fi
    echo "cloud_setup: attempt $attempt failed; retrying in ${delay}s: $*" >&2
    sleep "$delay"
    attempt=$((attempt + 1))
    delay=$((delay * 2))
  done
}

WARM_LOG="$HOME/.construct-cloud-warmup.log"
WARM_STATUS="$HOME/.construct-cloud-warmup.status"
NEXTEST_VERSION=0.9.145

warm_caches() {
  local failed=()
  echo "running $$" >"$WARM_STATUS"
  # Anything that ends the warm-up early (a signal, a crash) must not leave "running" behind.
  trap 'grep -q "^running" "$WARM_STATUS" 2>/dev/null && echo "failed interrupted" >"$WARM_STATUS"' EXIT
  (cd "$REPO_ROOT" && with_backoff cargo fetch) || failed+=(fetch)
  (cd "$REPO_ROOT" && cargo build -p construct-sim) || failed+=(construct-sim)
  # The test runner CI uses (same version as .github/workflows/ci.yml), built from crates.io.
  if ! cargo nextest --version 2>/dev/null | grep -q "$NEXTEST_VERSION"; then
    with_backoff cargo install --locked --quiet cargo-nextest --version "$NEXTEST_VERSION" || failed+=(nextest)
  fi
  if [[ ${#failed[@]} == 0 ]]; then
    echo done >"$WARM_STATUS"
  else
    echo "failed ${failed[*]}" >"$WARM_STATUS"
    echo "cloud_setup: WARNING: cache warm-up failed (${failed[*]}); the first build will download dependencies." >&2
  fi
}

warm_mode="${CONSTRUCT_SETUP_WARM:-background}"
case "$warm_mode" in
  foreground) warm_caches 2>&1 | tee "$WARM_LOG" ;;
  background)
    # setsid + nohup: detached from the setup script, so it keeps running after the script returns.
    export -f warm_caches with_backoff
    export REPO_ROOT WARM_STATUS NEXTEST_VERSION PATH
    setsid nohup bash -c warm_caches >"$WARM_LOG" 2>&1 </dev/null &
    # The child writes its own pid too; writing it here closes the gap before it starts.
    echo "running $!" >"$WARM_STATUS"
    echo "Cache warm-up running in the background: log $WARM_LOG, status $WARM_STATUS"
    ;;
  skip) ;;
  *) echo "cloud_setup: unknown CONSTRUCT_SETUP_WARM=$warm_mode (background|foreground|skip)" >&2; exit 2 ;;
esac

echo "Cloud setup done. rustc: $(rustc --version)"
