# The owner's own website draws their pod and public links by loading the web client from the server

Date: 2026-11-21. Related: [web-client](web-client.md), [public-web-links](public-web-links.md),
[public-row-links](public-row-links.md), [web-upstream-sign-in](web-upstream-sign-in.md),
[web-sessions-share-sign-ins](web-sessions-share-sign-ins.md), [mcp-connector](mcp-connector.md). Issue #365.

The owner wants their pod at their own site's `/construct` (signed in) and their public links at its `/events`,
`/recipes`, … (for anyone), while the site itself stays a lightweight static site (GitHub Pages) that the pod's
server doesn't host.

Considered:

- **The server hosts the domain** (DNS to Fly; the host routes `/construct`, `/<slug>`, and forwards everything
  else to the static site). Same origin, so the cookie works unchanged, but the homepage's uptime becomes the pod
  server's and every deploy of the server is a deploy of the homepage. The owner rejected it.
- **A full-page iframe of `/web`** (the embed of issue #365). The session would stay on the server's origin, but a
  framed page's cookie is a third-party cookie: it needs partitioned cookies (CHIPS), upstream sign-in pages refuse
  to be framed, and the address bar can't follow the app. Kept for embedding a public link in other people's
  pages (#365), not taken here.
- **A proxy in front of both** (a Cloudflare Worker). Works, but a third service, with the routing outside the repo.
- **The site loads the client from the server.** Taken.

Decision: **a site listed in `CONSTRUCT_SITE_ORIGINS` loads `/web/mount.js` from the server, which loads the web
client (always the server's own build) and draws in the site's page; the client calls the server across origins
with a bearer, never a cookie. The pod code is typed on the server's own page, which hands the site a single-use
code for a browser session.**

- **`CONSTRUCT_SITE_ORIGINS`** (`pod_host::site`): exact origins, `https://` (or `http://localhost` and
  `http://127.0.0.1` for development), comma-separated. Only with `CONSTRUCT_WEB=1`. Empty (the default): nothing
  below exists.
- **CORS**: a listed origin's preflights are answered by the host (204, the web client's headers, `Max-Age` 600)
  and its answers carry `Access-Control-Allow-Origin` and `Vary: Origin`; never `Allow-Credentials`, so the server's
  cookie never crosses. `/oauth/*` and `/web/session` stay same-origin. Any other origin gets no CORS headers.
- **Sign-in** (`/web/site`): the site's script sends the browser to `GET /web/site?origin&return&state&challenge`
  (a listed origin, a plain path on it, PKCE S256). The page asks for the pod code there, rate-limited like the code
  screen, and redirects (303) to `<origin><return>?construct_code=…&state=…`; its policy allows forms to post only
  to itself and that origin, and it can't be framed. The site trades the code once, within 5 minutes, at
  `POST /web/site/token {code, verifier}` from the same `Origin`, for `{session_token, expires_in}`: a new browser
  session with the pod code's sign-ins, the same `web` credential the cookie holds (decision
  `web-sessions-share-sign-ins`). It works as a bearer on every pod route but `/v1/mcp`, the pod answers it without
  session tokens as it does the cookie, and `POST /web/logout` with it ends it. The codes share `oauth_codes` under the
  client id `site:<origin>`, which the OAuth token endpoint refuses, so a site's code never becomes an MCP
  credential, and the site endpoint takes no other code.
- **The client** (`construct_web::connect(server, token, page)`): before `start` or `start_public`, every pod path
  goes to `server` with `token`; `page` (the site's path) is where it returns when signed out and the redirect of an
  upstream sign-in (decision `web-upstream-sign-in`: the site's page must be in `PIXELFED_EXTRA_REDIRECTS`), and on a
  public link the base of the addresses rows get (`/recipes/<row>`).
- **`mount.js`** (unhashed, revalidated; it names the hashed glue and the fonts' hash as `app.js` does): draws into
  `#construct-root` (`data-mode="app"`), or the public link `/<handle>/<slug>[/<row>]` for the site's
  `/<slug>[/<row>]` (`data-mode="public" data-handle="<handle>"`). It keeps the session in the site's
  `localStorage`, checks it on load (a 401 forgets it and asks again), and leaves `?code=&state=` to the client's
  upstream sign-in.

Accepted: the site's origin holds a bearer for the pod, so any script on that site could use it. The owner's site
loads no third-party scripts, and its pages set a Content-Security-Policy that allows scripts only from itself and
the server; a site that adds analytics or ads should keep them off the pod's page or give the pod its own
subdomain. A static host without rewrites (GitHub Pages) serves a row's address (`/recipes/<row>`) from its 404
page, so those answer 404 and link previews show the site's own title.

Not yet: a site's page for a single public link (`data-mode="public"` maps a path; a fixed link per page would need
`data-slug`); embeds sized to their content inside a page (issue #365); a site per pod (the list is the server's,
so every pod on the server may sign in from a listed site, each with its own code).
