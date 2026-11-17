# The web client is downloaded once per deploy, not on every load

Date: 2026-11-17. Related: [web-client](web-client.md), [web-on-staging](web-on-staging.md),
[public-web-links](public-web-links.md).

`/web` and `/s/<link>` served the client's files (`app.js`, `style.css`, `construct_web.js`, `construct_web_bg.wasm`)
under fixed names with `no-cache` and no validator, so every page load downloaded the engine again (about 2 MB of
gzipped WebAssembly). Fixed names can't be cached for long without serving old code after a deploy.

Considered:

- **An `ETag` on everything**: each load still asks for every file (a round trip each, the fonts included), only
  smaller answers.
- **A manifest the server reads** (`manifest.json` mapping names to hashed names, filled into the page): the server
  learns about the build's files. Not needed: the build can rewrite the few references itself.
- **Hashed names written by the build.** Taken.

Decision: **`scripts/web_build.sh` names the script, the WebAssembly, its glue and the style by a hash of their content
(`app.<16 hex>.js`, …) and rewrites the names that refer to them (the glue's `.wasm`, `app.js`'s import, the page's
script and style); the server keeps a hashed name for good and revalidates the rest.**

- **Hashed names** (`<stem>.<16 lowercase hex>.<ext>`) are served `public, max-age=31536000, immutable`. A deploy
  changes the names the page asks for, so the next load takes the new client.
- **Fonts** keep their names (the client asks for them by name); the build puts a hash of all of them in `app.js`
  (`constructFontVersion`) and the client asks for `/web/fonts/<file>?v=<hash>`, also `immutable`.
- **Everything else** (the pages `/web` and `/s/<link>`, a name without a hash) is `no-cache` with an `ETag` (a hash of
  what is sent), answered `304` when `If-None-Match` names it.
- The served directory stays static (`CONSTRUCT_WEB_DIR`); the build removes a previous build's files.

Browsers keep their cache per top-level site, so this is one download per deploy per site. A tab left open across a
deploy keeps working until it reloads (it has its files); a stale tab that lazily asked for a file of the old build
would get a 404, which this client doesn't do (it loads everything at start).
