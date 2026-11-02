#!/usr/bin/env bash
# End-to-end suite (crates/e2e/tests/suite.rs): sign-in via PKCE with a scripted browser, feed,
# like, offline like -> flush, bookmark; the real pod and client-core engine in-process.
# README "End-to-end tests".
#
# Usage: scripts/e2e.sh [--upstream fake|real] [--in-process] [--faults] [-- <cargo test args>]
#   --upstream fake  (default) build the fake-pixelfed binary, start it on a free loopback port
#                    with its admin endpoints, run the suite against it (E2E_FAKE_URL), stop it.
#     --in-process   use the fake in-process instead (what `cargo test --workspace` does).
#     --faults       also run the upstream fault tests (crates/e2e/tests/upstream_faults.rs,
#                    in-process fake; ~11 s).
#   --upstream real  a real Pixelfed: PIXELFED_INSTANCE_URL (default http://localhost:8090, the
#                    local dev stack, scripts/pixelfed_dev.sh up && seed). Signs in through the
#                    web login as E2E_PIXELFED_LOGIN/E2E_PIXELFED_PASSWORD (default
#                    alice/alice-dev-pass) and checks upstream state with E2E_PIXELFED_TOKEN
#                    (default dev/pixelfed/.seed/<login>_token). Undoes its like/bookmark.
#                    Use a throwaway instance: each run registers one OAuth app on it.
# Env: E2E_FAKE_URL (fake mode: use an already running fake, e.g. its Docker image, instead).
set -euo pipefail

cd "$(dirname "$0")/.."
upstream="${E2E_UPSTREAM:-fake}"
in_process=0
faults=0
extra=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --upstream) upstream="${2:?--upstream needs fake or real}"; shift 2 ;;
    --upstream=*) upstream="${1#*=}"; shift ;;
    --in-process) in_process=1; shift ;;
    --faults) faults=1; shift ;;
    --) shift; extra=("$@"); break ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1 (see --help)" >&2; exit 2 ;;
  esac
done

run_suite() {
  cargo test -p e2e --test suite -- --nocapture ${extra[@]+"${extra[@]}"}
}

case "$upstream" in
  fake)
    export E2E_UPSTREAM=fake
    if [[ $in_process == 1 ]]; then
      unset E2E_FAKE_URL
      run_suite
    elif [[ -n "${E2E_FAKE_URL:-}" ]]; then
      echo "e2e: using the fake at $E2E_FAKE_URL"
      run_suite
    else
      cargo build -q -p fake-pixelfed
      log="$(mktemp -t fake-pixelfed.XXXXXX)"
      FAKE_PIXELFED_ADDR=127.0.0.1:0 FAKE_PIXELFED_ADMIN=1 RUST_LOG="${RUST_LOG:-info}" \
        target/debug/fake-pixelfed >"$log" 2>&1 &
      pid=$!
      trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true' EXIT
      url=""
      for _ in $(seq 100); do
        url="$(grep -o 'listening on http://[0-9.:]*' "$log" | head -n1 | sed 's/listening on //' || true)"
        [[ -n "$url" ]] && break
        kill -0 "$pid" 2>/dev/null || { echo "fake-pixelfed exited:" >&2; cat "$log" >&2; exit 1; }
        sleep 0.1
      done
      [[ -n "$url" ]] || { echo "fake-pixelfed did not start:" >&2; cat "$log" >&2; exit 1; }
      echo "e2e: fake-pixelfed (pid $pid) at $url, log $log"
      E2E_FAKE_URL="$url" run_suite
    fi
    if [[ $faults == 1 ]]; then
      cargo test -p e2e --test upstream_faults ${extra[@]+"${extra[@]}"}
    fi
    ;;
  real)
    export E2E_UPSTREAM=real
    export PIXELFED_INSTANCE_URL="${PIXELFED_INSTANCE_URL:-http://localhost:8090}"
    if ! curl -fsS -o /dev/null "$PIXELFED_INSTANCE_URL/api/v1/instance"; then
      echo "e2e: no Pixelfed at $PIXELFED_INSTANCE_URL (scripts/pixelfed_dev.sh up && scripts/pixelfed_dev.sh seed)" >&2
      exit 1
    fi
    echo "e2e: real Pixelfed at $PIXELFED_INSTANCE_URL as ${E2E_PIXELFED_LOGIN:-alice}"
    run_suite
    ;;
  *) echo "--upstream must be fake or real, not '$upstream'" >&2; exit 2 ;;
esac
