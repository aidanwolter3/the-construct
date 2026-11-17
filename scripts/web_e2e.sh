#!/usr/bin/env bash
# Builds the web client and the server, starts the server with CONSTRUCT_WEB=1 and CONSTRUCT_PUBLIC_WEB=1 on a
# spare port with a throwaway pod and the fake Pixelfed on another, and drives the client in Chromium: the owner's
# pod (crates/construct-web/tests/e2e.js, which also signs in to the fake Pixelfed and rebuilds the client to see a
# reload take the new one) and a public link (public_e2e.js). Needs Node and Playwright, which cloud sessions have.
# Screenshots go to <out> (default target/web-e2e). Decisions `web-client`, `public-web-links`, `web-client-cache`,
# `web-upstream-sign-in`.
#   scripts/web_e2e.sh [out]
set -euo pipefail
cd "$(dirname "$0")/.."
out=${1:-target/web-e2e}
mkdir -p "$out"
scripts/web_build.sh target/web >/dev/null
cargo build -q -p server -p fake-pixelfed
data=$(mktemp -d)
spare() { python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'; }
port=$(spare)
fport=$(spare)
FAKE_PIXELFED_ADDR=127.0.0.1:$fport target/debug/fake-pixelfed >"$out/fake-pixelfed.log" 2>&1 &
fake=$!
code=cpod_webe2e$(python3 -c 'import secrets; print(secrets.token_hex(8))')
key=cmcp_webe2e$(python3 -c 'import secrets; print(secrets.token_hex(8))')
CONSTRUCT_DATA_DIR=$data CONSTRUCT_ADDR=127.0.0.1:$port CONSTRUCT_WEB=1 CONSTRUCT_WEB_DIR=target/web \
  CONSTRUCT_PUBLIC_WEB=1 CONSTRUCT_OPEN_POD=off PIXELFED_INSTANCE_URL=http://127.0.0.1:$fport \
  PIXELFED_EXTRA_REDIRECTS=http://127.0.0.1:$port/web target/debug/server serve --pod-code "$code" --mcp-key "$key" \
  --pod-name webe2e >"$out/server.log" 2>&1 &
server=$!
trap 'kill $server $fake 2>/dev/null; rm -rf "$data"' EXIT
for _ in $(seq 1 100); do
  curl -fsS "http://127.0.0.1:$port/healthz" >/dev/null 2>&1 && break
  python3 -c 'import time; time.sleep(0.2)'
done
export NODE_PATH=${NODE_PATH:-$(npm root -g)}
node crates/construct-web/tests/e2e.js "http://127.0.0.1:$port" "$code" "$out" \
  "CONSTRUCT_WEB_STAMP=e2e-$$ scripts/web_build.sh target/web >/dev/null"
node crates/construct-web/tests/public_e2e.js "http://127.0.0.1:$port" "$key" "$out"
