# The web client is the one renderer in a browser, signed in with a session cookie

Date: 2026-11-17. Related: [screen-sizes](screen-sizes.md), [rust-clients](rust-clients.md),
[native-client-pod-codes](native-client-pod-codes.md), [mcp-connector](mcp-connector.md), [sharing](sharing.md).

People should be able to open their pod in a browser: visit a page, type the pod code once, and see the pod laid out
for a large window. It is also the way to public links later (a page for people without a pod). It must not change
anything for today's pods, apps or staging while it is a prototype.

Considered:

- **The pod renders HTML** (each node to HTML and CSS). Text selection, screen readers, search engines and fast first
  paint for free, but a second renderer that drifts from the one every app draws with. Rejected for now; it may come
  back for public pages only.
- **The engine (`client-core`) in WebAssembly**: offline cache and outbox in the browser too, but its I/O is blocking
  HTTP, threads and SQLite, a rewrite that touches the native apps. Deferred.
- **The renderer in WebAssembly over a browser loader, drawn on the CPU into a canvas.** Taken.

Decision: **`crates/construct-web` is `construct-render` compiled to WebAssembly (without `gpu` or `remote`), drawn by
its CPU rasterizer into a `<canvas>`, over the pod's HTTP API on the page's own origin.**

- **The loader** (`construct_web::web::WebLoader`, a `Loader`) holds the construct: home's root and the objects it
  reaches (`POST /v1/objects`), so a layout is materialized in the browser at once (`construct_web::construct`, no
  I/O, tested natively). A browser can't block, so data comes in the background: `layout` answers what it has and
  fetches the data, which `changes` brings; `fetch` and `send` answer through their callbacks. The events stream is
  read from a `fetch` body (not `EventSource`, which can't send the features header); a `construct` event moves the
  open layouts to the new root, a `data` event reloads their data.
- **The renderer in a browser**: one clock module (`construct_render::time`, std's on native and `web_time`'s in a
  browser, where std's panic), jiff with its browser time zone and bundled database, images cut on the caller's
  thread, `local://device#platform` `web`. Fonts are not built in: the client fetches the faces before its first
  frame and the fallbacks (symbols, emoji) after it, and a text engine loads fonts added later and measures again.
  Native builds are unchanged: these are `cfg(target_arch = "wasm32")` or additive.
- **Signing in is a cookie that is not the pod code.** A pod code is a pinned session that never expires
  (decision `native-client-pod-codes`), so it must not sit in a browser. The page posts the code once to
  `POST /web/session`; the host checks it as `POST /v1/pods/open` does (same rate limit), makes an ordinary session
  of the pod (it expires after `CONSTRUCT_SESSION_TTL_DAYS`; logging out deletes it) and a host credential of a new
  kind, `web` (`host.db` migration `web_credentials`), and answers it as an `HttpOnly; SameSite=Strict` cookie. The
  host turns the cookie into that session's bearer for the pod. Rotating the pod code ends every browser's session.
- **The cookie is scoped**: taken only with `CONSTRUCT_WEB=1`, only with `x-construct-client: web` on the request
  (a page of another site can't send it without a preflight, which nothing answers) and, where the browser sends
  `Sec-Fetch-Site`, `same-origin`; only as a `web` credential (a pod code in the cookie is not taken); never for
  `/v1/mcp`. The page allows only its own scripts and WebAssembly (no inline script).
- **Images of other sites** go through `GET /web/image` (a canvas can't read pixels without CORS): with the cookie,
  public `http(s)` hosts only (names that resolve to private addresses are refused when connecting), images only, at
  most 10 MiB.
- **Features**: the client sends the renderer's features minus what a browser can't do yet (`media_picker`,
  `auth_login`, `device_notification_permission`), so the pod serves older versions without them.
- **Off by default**: `/web`, the cookie and the proxy exist only with `CONSTRUCT_WEB=1` (unset on staging), from
  files `scripts/web_build.sh` writes to `CONSTRUCT_WEB_DIR`; nothing is built into the server. CI builds the client
  and keeps the gzipped WebAssembly under 2.5 MiB (about 1.9 today).

Measured: drawing a 1280 × 800 window takes about 50 ms on the CPU, 90 ms at twice the density, so a long page
scrolls at 10 to 20 frames a second.

Not yet: the GPU path in the browser (wgpu's WebGL2 or WebGPU backend) for smooth scrolling; drags of cards, pagers
and sheets; photo picking; upstream sign-ins; notifications and an offline cache; an accessibility tree for screen
readers; serving it on staging. Public links for people without a pod are a later decision over this client.
