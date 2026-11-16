#!/usr/bin/env bash
# The grep gate of issues #187 and #300 (decision `one-layout-system`): the old layout system's names may appear
# under crates/ only in migration code (a path containing "migrations") and in crates/protocol/features/removed/.
set -euo pipefail
cd "$(dirname "$0")/.."

pattern='custom_apps|app_versions|core://Ref|pin_version|local:layouts'
hits=$(grep -rnE "$pattern" crates/ | grep -vE '^[^:]*migrations[^:]*:|^crates/protocol/features/removed/' || true)
if [ -n "$hits" ]; then
  echo "Old layout-system names outside migrations (decision one-layout-system):" >&2
  echo "$hits" >&2
  exit 1
fi
