#!/usr/bin/env bash
# Regenerates every app icon asset from one geometry definition (scripts/gen_icons.py):
#   assets/icon/*.svg, assets/icon/png/construct-512.png, assets/icon/construct.{icns,ico},
#   Android adaptive icon (res/drawable/ic_launcher_*.xml, mipmap-*), the macOS .icns (scripts/macos_app.sh bundles it).
# Needs python3 with Pillow and numpy; iconutil (macOS) for the .icns.
set -euo pipefail
cd "$(dirname "$0")/.."
python3 -c "import PIL, numpy" 2>/dev/null || { echo "need python3 with Pillow and numpy (pip install pillow numpy)" >&2; exit 1; }
exec python3 scripts/gen_icons.py "$@"
