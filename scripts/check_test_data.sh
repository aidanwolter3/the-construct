#!/usr/bin/env bash
# Checks that test data stays out of the pod (decision `data-next-to-its-code`): Rust code under a crate's src/
# may embed a file from a tests/ folder (include_str!/include_bytes!) only from test-only code, that is inside a
# `#[cfg(test)]` module or right under a `#[cfg(test)]` / `#[cfg(any(test, …))]` attribute.
set -euo pipefail
cd "$(dirname "$0")/.."

status=0
while IFS= read -r f; do
  # Prints "line: text" for each embed of a tests/ file outside test-only code.
  bad=$(awk '
    /^[[:space:]]*#\[cfg\(test\)\]/ { attr = 1; next }
    /^[[:space:]]*#\[cfg\(any\(test/ { attr = 1; next }
    attr && /^[[:space:]]*(pub[[:space:]]+)?mod[[:space:]]/ { in_tests = 1 }
    /include_(str|bytes)!\(.*tests\// { if (!in_tests && !attr) print FNR ": " $0 }
    /^[[:space:]]*(\/\/|#\[)/ { next }
    { attr = 0 }
  ' "$f")
  if [ -n "$bad" ]; then
    while IFS= read -r line; do
      echo "$f:$line" >&2
    done <<<"$bad"
    echo "$f: embeds test data in non-test code; gate it with #[cfg(test)] or a test-only feature." >&2
    status=1
  fi
done < <(find crates fake-pixelfed -path '*/target' -prune -o -path '*/src/*.rs' -print)

exit $status
