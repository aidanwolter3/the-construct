#!/usr/bin/env bash
# Setup for Claude Code cloud environments (Ubuntu containers): toolchains for cargo tests, the JVM tests and
# the headless simulator, plus warm caches so a session doesn't download hundreds of artifacts on its first build.
# Use it as the environment's setup script (paste the snippet from CLAUDE.md "Cloud sessions" into the
# environment settings), or run it once in a session: `bash scripts/cloud_setup.sh`. Idempotent.
#
# Cache warm-up: builds client-ffi (target/release/libclient_ffi.so) and compiles the Kotlin main + test sources,
# which fills ~/.gradle with every dependency the JVM tests and the simulator need. Gradle tries Google's Maven
# Central mirror before repo.maven.apache.org (kmp-app/settings.gradle.kts), because Maven Central answers
# HTTP 429 to shared cloud egress IPs. A failed step is retried with long, growing waits (30s..4min), never in a
# tight loop, and a warm-up that still fails only prints a warning: sessions then download on first build.
# Env: CONSTRUCT_SETUP_SKIP_WARM=1 skips the warm-up (toolchains only).
#
# Network access the environment needs (allow these domains in the environment's network policy):
#   static.rust-lang.org, sh.rustup.rs, crates.io, index.crates.io, static.crates.io   (Rust)
#   maven-central.storage-download.googleapis.com, repo.maven.apache.org, repo1.maven.org,
#   dl.google.com, maven.google.com, plugins.gradle.org, services.gradle.org, downloads.gradle.org,
#   maven.pkg.jetbrains.space, packages.jetbrains.team, download.jetbrains.com         (Gradle/Kotlin/Compose)
#   github.com, api.github.com, objects.githubusercontent.com                          (git, gh)
#   the-construct-pod.fly.dev, the-construct-fake-pixelfed.fly.dev                     (staging checks)
# Not needed in the cloud: Android SDK/emulator, macOS tools (device QA happens on the developer's Mac).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SUDO=""
if [[ $EUID -ne 0 ]] && command -v sudo >/dev/null; then SUDO="sudo"; fi

if command -v apt-get >/dev/null; then
  need=()
  command -v java >/dev/null && java -version 2>&1 | grep -q '"21' || need+=(openjdk-21-jdk-headless)
  command -v cc >/dev/null || need+=(build-essential)
  command -v jq >/dev/null || need+=(jq)
  command -v sqlite3 >/dev/null || need+=(sqlite3)
  dpkg -s fonts-dejavu-core >/dev/null 2>&1 || need+=(fonts-dejavu-core fontconfig)  # simulator text rendering
  dpkg -s pkg-config >/dev/null 2>&1 || need+=(pkg-config)
  if [[ ${#need[@]} -gt 0 ]]; then
    $SUDO apt-get update -y
    DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y "${need[@]}"
  fi
fi

if ! command -v cargo >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --component clippy,rustfmt
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
else
  rustup component add clippy rustfmt >/dev/null 2>&1 || true
fi

JAVA_HOME_GUESS="$(dirname "$(dirname "$(readlink -f "$(command -v java)")")")"

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

warm_ok=1
if [[ "${CONSTRUCT_SETUP_SKIP_WARM:-}" != "1" ]]; then
  (cd "$REPO_ROOT" && with_backoff cargo build -p client-ffi --release) || warm_ok=0
  # Compiling main + test sources resolves everything :shared:allTests, :desktopApp:test and :desktopApp:simulate
  # use. --no-daemon so no Gradle daemon outlives the setup script.
  (cd "$REPO_ROOT/kmp-app" && with_backoff ./gradlew -Dorg.gradle.java.home="$JAVA_HOME_GUESS" -PskipRustBuild \
    --no-daemon --quiet :shared:compileTestKotlinDesktop :desktopApp:compileTestKotlin) || warm_ok=0
fi

cat <<EOF
Cloud setup done.
  rustc:  $(rustc --version)
  java:   $(java -version 2>&1 | head -n 1)
Export for Gradle (kmp-app/gradle.properties pins a macOS JDK path; override it):
  export JAVA_HOME=$JAVA_HOME_GUESS
  ./gradlew -Dorg.gradle.java.home="\$JAVA_HOME" -PskipRustBuild ...
EOF
if [[ $warm_ok != 1 ]]; then
  echo "cloud_setup: WARNING: cache warm-up failed (see above); the first build in a session will download dependencies." >&2
fi
