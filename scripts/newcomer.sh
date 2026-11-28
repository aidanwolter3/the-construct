#!/usr/bin/env bash
# The newcomer test (decision `newcomer-journeys`, docs/testing.md "Newcomer test"): for each persona in
# crates/newcomer/personas, a new temporary pod on staging, an agent with nothing but its MCP tools building what
# the persona asks for, then the simulator and a reviewer checking the result.
#
#   scripts/newcomer.sh [out dir] [newcomer flags: --only groceries,reading --base-url <url> ...]
#
# Needs `claude` on the path (Claude Code, signed in) and network access to the pod server. Writes
# <out>/report.json and <out>/report.md, and prints the report's path. Pod tokens stay in memory.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$root/target/newcomer/$(date -u +%Y-%m-%dT%H%M)}
shift || true
cd "$root"
command -v claude >/dev/null || { echo "newcomer.sh: claude (Claude Code) is not on the path" >&2; exit 2; }
cargo build -q -p construct-sim -p newcomer
exec target/debug/newcomer --out "$out" --sim target/debug/construct-sim "$@"
