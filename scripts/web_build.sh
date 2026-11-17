#!/usr/bin/env bash
# Builds the web client (decision `web-client`, docs/clients.md#web) into <out> (default target/web), which the server
# serves at /web with CONSTRUCT_WEB=1 (CONSTRUCT_WEB_DIR=<out>): index.html, and the script, WebAssembly and style
# named by a hash of their content (app.<hash>.js, construct_web.<hash>.js, construct_web_bg.<hash>.wasm,
# style.<hash>.css), which the server lets browsers keep for good (decision `web-client-cache`); fonts/ and their hash
# in app.js. Needs the wasm32-unknown-unknown target and the wasm-bindgen CLI at the version
# in Cargo.lock: rustup target add wasm32-unknown-unknown; cargo install wasm-bindgen-cli --version <it> --locked
#   scripts/web_build.sh [out] [--dev]
set -euo pipefail
cd "$(dirname "$0")/.."
out=${1:-target/web}
profile=web-release
[ "${2:-}" = "--dev" ] && profile=dev
want=$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | grep version | cut -d'"' -f2)
have=$(wasm-bindgen --version 2>/dev/null | cut -d' ' -f2 || true)
if [ "$want" != "$have" ]; then
  echo "wasm-bindgen CLI $want is needed (have: ${have:-none}): cargo install wasm-bindgen-cli --version $want --locked" >&2
  exit 1
fi
cargo build -q -p construct-web --target wasm32-unknown-unknown --profile "$profile"
dir=$([ "$profile" = dev ] && echo debug || echo "$profile")
mkdir -p "$out"
# A previous build's files: their names change with their content.
rm -f "$out"/*.js "$out"/*.wasm "$out"/*.css "$out"/*.html
wasm-bindgen --target web --no-typescript --remove-name-section --remove-producers-section --out-dir "$out" "target/wasm32-unknown-unknown/$dir/construct_web.wasm"
# The page, and the renderer's fonts, which the client fetches instead of carrying.
cp crates/construct-web/static/* "$out/"
# A build that differs from the last without a change to the code (scripts/web_e2e.sh's reload after a rebuild).
[ -n "${CONSTRUCT_WEB_STAMP:-}" ] && echo "// build $CONSTRUCT_WEB_STAMP" >> "$out/app.js"
mkdir -p "$out/fonts"
cp crates/construct-render/fonts/*.ttf "$out/fonts/"

# Content hashes: each file is renamed after its hash once the names it refers to are final (the wasm, then the
# glue that loads it, then app.js that imports the glue and holds the fonts' hash, then the page).
hash() { sha256sum "$@" | cut -c1-16; }
hashed() { # <file> <stem> <ext>: renames it to <stem>.<hash>.<ext> and prints the new name
  local n; n="$2.$(hash "$out/$1").$3"
  mv "$out/$1" "$out/$n"
  echo "$n"
}
grep -q "'construct_web_bg.wasm'" "$out/construct_web.js" || { echo "the glue doesn't name construct_web_bg.wasm" >&2; exit 1; }
wasm=$(hashed construct_web_bg.wasm construct_web_bg wasm)
sed -i "s|'construct_web_bg.wasm'|'$wasm'|" "$out/construct_web.js"
glue=$(hashed construct_web.js construct_web js)
fonts=$(cat "$out"/fonts/*.ttf | hash)
sed -i -e "s|\"/web/construct_web.js\"|\"/web/$glue\"|" -e "s|\"FONT_VERSION\"|\"$fonts\"|" "$out/app.js"
grep -q "/web/$glue" "$out/app.js" && grep -q "\"$fonts\"" "$out/app.js" || { echo "app.js wasn't rewritten" >&2; exit 1; }
app=$(hashed app.js app js)
style=$(hashed style.css style css)
sed -i -e "s|\"/web/app.js\"|\"/web/$app\"|" -e "s|\"/web/style.css\"|\"/web/$style\"|" "$out/index.html"
grep -q "/web/$app" "$out/index.html" && grep -q "/web/$style" "$out/index.html" || { echo "index.html wasn't rewritten" >&2; exit 1; }
ls -l "$out"
