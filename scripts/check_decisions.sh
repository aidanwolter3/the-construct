#!/usr/bin/env bash
# Checks the decision records (docs/decisions/README.md): docs/DECISIONS.md is closed at D52, and every
# docs/decisions/<name>.md has a lowercase-hyphenated name and a "# " title on its first line.
set -euo pipefail
cd "$(dirname "$0")/.."

status=0
last=52

while IFS= read -r line; do
  n=${line#"## D"}
  n=${n%%:*}
  if [ "$n" -gt "$last" ]; then
    echo "docs/DECISIONS.md: new entry '$line'. New decisions go in docs/decisions/<name>.md (see its README)." >&2
    status=1
  fi
done < <(grep -E '^## D[0-9]+:' docs/DECISIONS.md)

for f in docs/decisions/*.md; do
  name=$(basename "$f" .md)
  [ "$name" = README ] && continue
  if ! [[ "$name" =~ ^[a-z0-9]+(-[a-z0-9]+)*$ ]]; then
    echo "$f: name must be lowercase words joined by hyphens." >&2
    status=1
  fi
  if ! IFS= read -r first < "$f" || [[ "$first" != "# "* ]]; then
    echo "$f: first line must be a '# ' title." >&2
    status=1
  fi
done

exit $status
