#!/usr/bin/env bash
# Prints this checkout's build id (decision `device-versions`):
# <version>+<commit time, UTC, YYYYMMDDHHMMSS>.<short hash>[.dirty], e.g. 0.6.0+20261118153000.c41d9e2.
# The server, the web client and the apps compile it in from CONSTRUCT_BUILD, so the pod can tell which devices run
# the server's build, an older one, or a newer one (an app built before its deploy landed). The version is
# CONSTRUCT_VERSION_NAME when set (a release), else the workspace's; the commit time orders builds.
set -euo pipefail
cd "$(dirname "$0")/.."
version=${CONSTRUCT_VERSION_NAME:-$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)}
stamp=$(TZ=UTC git log -1 --date=format-local:%Y%m%d%H%M%S --format=%cd)
hash=$(git rev-parse --short=7 HEAD)
dirty=
git diff --quiet HEAD -- 2>/dev/null || dirty=.dirty
echo "${version}+${stamp}.${hash}${dirty}"
