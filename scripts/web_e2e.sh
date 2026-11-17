#!/usr/bin/env bash
# Builds the web client and the server, starts the server with CONSTRUCT_WEB=1 on a spare port with a throwaway
# pod, and drives the client in Chromium (crates/construct-web/tests/e2e.js; needs Node and Playwright, which
# cloud sessions have). Screenshots go to <out> (default target/web-e2e). Decision `web-client`.
#   scripts/web_e2e.sh [out]
set -euo pipefail
cd "$(dirname "$0")/.."
out=${1:-target/web-e2e}
mkdir -p "$out"
scripts/web_build.sh target/web >/dev/null
cargo build -q -p server
data=$(mktemp -d)
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
code=cpod_webe2e$(python3 -c 'import secrets; print(secrets.token_hex(8))')
CONSTRUCT_DATA_DIR=$data CONSTRUCT_ADDR=127.0.0.1:$port CONSTRUCT_WEB=1 CONSTRUCT_WEB_DIR=target/web \
  CONSTRUCT_OPEN_POD=off target/debug/server serve --pod-code "$code" --pod-name webe2e >"$out/server.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null; rm -rf "$data"' EXIT
for _ in $(seq 1 100); do
  curl -fsS "http://127.0.0.1:$port/healthz" >/dev/null 2>&1 && break
  python3 -c 'import time; time.sleep(0.2)'
done
NODE_PATH=${NODE_PATH:-$(npm root -g)} node crates/construct-web/tests/e2e.js "http://127.0.0.1:$port" "$code" "$out"
