#!/usr/bin/env bash
# Setup for Claude Code cloud environments (Ubuntu containers): toolchains for cargo tests, the JVM tests and
# the headless simulator. Use it as the environment's setup script (Claude Code on the web: environment
# settings), or run it once in a session: `bash scripts/cloud_setup.sh`. Idempotent.
#
# Network access the environment needs (allow these domains in the environment's network policy):
#   static.rust-lang.org, sh.rustup.rs, crates.io, index.crates.io, static.crates.io   (Rust)
#   repo.maven.apache.org, repo1.maven.org, dl.google.com, maven.google.com,
#   plugins.gradle.org, services.gradle.org, downloads.gradle.org,
#   maven.pkg.jetbrains.space, packages.jetbrains.team, download.jetbrains.com         (Gradle/Kotlin/Compose)
#   github.com, api.github.com, objects.githubusercontent.com                          (git, gh)
#   the-construct-pod.fly.dev, the-construct-fake-pixelfed.fly.dev                     (staging checks)
# Not needed in the cloud: Android SDK/emulator, macOS tools (device QA happens on the developer's Mac).

set -euo pipefail

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
cat <<EOF
Cloud setup done.
  rustc:  $(rustc --version)
  java:   $(java -version 2>&1 | head -n 1)
Export for Gradle (kmp-app/gradle.properties pins a macOS JDK path; override it):
  export JAVA_HOME=$JAVA_HOME_GUESS
  ./gradlew -Dorg.gradle.java.home="\$JAVA_HOME" -PskipRustBuild ...
EOF
