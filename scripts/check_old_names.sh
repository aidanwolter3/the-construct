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

# The word "blueprint" (decision `element-roles-and-reuse`): what ships with the server is a core layout or a
# core component. It may stay in migration code, in the name of decision `blueprints-and-home`, and as the one
# alias `list_layouts` still answers for agents that haven't refreshed.
hits=$(grep -rniE 'blueprint' --include='*.rs' crates/ \
  | grep -vE '^[^:]*migrations[^:]*:' \
  | grep -vE 'blueprints-and-home|"blueprints": core_layouts|The word before decision' || true)
if [ -n "$hits" ]; then
  echo "\"blueprint\" in code (decision element-roles-and-reuse: say core layout):" >&2
  echo "$hits" >&2
  exit 1
fi
