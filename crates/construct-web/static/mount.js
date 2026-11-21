// The web client on the owner's own site (decision `site-origins`): a page of another origin, listed in the server's
// CONSTRUCT_SITE_ORIGINS, loads this module from the server and draws a pod, or a public link, in its own page:
//
//   <div id="construct-root" data-mode="app"></div>                       the owner's pod, after a sign-in
//   <div id="construct-root" data-mode="public" data-handle="ann"></div>  /<slug>[/<row>] of @ann's public links
//   <script type="module" src="https://<server>/web/mount.js"></script>
//
// The pod code is typed on the server's own page (/web/site), never this one: it sends the browser back here with a
// single-use code, which this module trades (with its PKCE verifier) for a browser session, kept in this site's
// localStorage. The engine and its fonts come from the server, so they always match it.

// scripts/web_build.sh rewrites these to the build's hashed names and the fonts' hash (decision `web-client-cache`).
const GLUE = "/web/construct_web.js";
globalThis.constructFontVersion = "FONT_VERSION";

const server = new URL(import.meta.url).origin;
const SESSION = "construct.site.session";
const PENDING = "construct.site.pending";
const web = { "x-construct-client": "web" };

const root = document.getElementById("construct-root") || document.body;
const mode = root.dataset.mode === "public" ? "public" : "app";

// The page's parts: the canvas the client draws in, the hidden input it types into, a status line and a button.
function part(tag, id) {
  let el = document.getElementById(id);
  if (!el) {
    el = document.createElement(tag);
    el.id = id;
    root.appendChild(el);
  }
  return el;
}
const status = part("p", "construct-status");
status.setAttribute("role", "status");
const button = part("button", "construct-sign-in");
button.type = "button";
button.hidden = true;
const canvas = part("canvas", "construct");
canvas.setAttribute("aria-label", mode === "app" ? "Your pod" : "A shared page");
const input = part("input", "construct-input");
input.setAttribute("aria-hidden", "true");
input.autocomplete = "off";
input.setAttribute("autocapitalize", "off");
input.spellcheck = false;

function say(text) {
  status.textContent = text;
  status.hidden = !text;
}

const store = (kind) => {
  try {
    return kind === "local" ? window.localStorage : window.sessionStorage;
  } catch {
    return null;
  }
};

function b64url(bytes) {
  return btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
const random = (n) => b64url(crypto.getRandomValues(new Uint8Array(n)));
async function challenge(verifier) {
  const d = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier));
  return b64url(new Uint8Array(d));
}

async function engine() {
  const glue = await import(new URL(GLUE, import.meta.url).href);
  await glue.default();
  return glue;
}

// Back from /web/site with ?construct_code=…&state=…: the code for a session. Other parameters (an upstream sign-in's
// ?code=&state=) are the client's own.
async function finishSignIn() {
  const q = new URLSearchParams(location.search);
  const code = q.get("construct_code");
  if (!code) return;
  history.replaceState(null, "", location.pathname);
  const raw = store("session")?.getItem(PENDING);
  store("session")?.removeItem(PENDING);
  const pending = raw ? JSON.parse(raw) : null;
  if (!pending || pending.state !== q.get("state")) {
    say("That sign-in didn't come from this page. Try again.");
    return;
  }
  const r = await fetch(`${server}/web/site/token`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ code, verifier: pending.verifier }),
  });
  const v = r.ok ? await r.json() : null;
  if (v?.session_token) {
    store("local")?.setItem(SESSION, v.session_token);
  } else {
    say("That sign-in didn't work. Try again.");
  }
}

// The session this site keeps, if the server still takes it.
async function session() {
  const token = store("local")?.getItem(SESSION);
  if (!token) return null;
  const r = await fetch(`${server}/v1/auth/sign-ins`, {
    headers: { ...web, authorization: `Bearer ${token}` },
  });
  if (r.status === 401 || r.status === 403) {
    store("local")?.removeItem(SESSION);
    return null;
  }
  return token;
}

async function signIn() {
  const state = random(24);
  const verifier = random(32);
  store("session")?.setItem(PENDING, JSON.stringify({ state, verifier }));
  const q = new URLSearchParams({
    origin: location.origin,
    return: location.pathname,
    state,
    challenge: await challenge(verifier),
  });
  location.assign(`${server}/web/site?${q}`);
}

async function app() {
  await finishSignIn();
  const token = await session();
  if (!token) {
    if (!status.textContent) say("");
    button.textContent = "Open my pod";
    button.hidden = false;
    button.onclick = () => signIn().catch((e) => say("Couldn't start the sign-in: " + e));
    return;
  }
  say("Opening your pod…");
  const glue = await engine();
  glue.connect(server, token, location.pathname);
  await glue.start();
  say("");
}

// /<slug>[/<row>] on the site is the public link /<handle>/<slug>[/<row>] (decision `public-row-links`).
async function shared() {
  const handle = root.dataset.handle || "";
  const parts = location.pathname.split("/").filter(Boolean);
  const [slug = "", row] = parts;
  const ok = (s) => /^[a-z0-9][a-z0-9_-]*$/.test(s);
  if (!ok(handle) || !ok(slug) || (row !== undefined && !ok(row)) || parts.length > 2) {
    say("There's nothing here.");
    return;
  }
  say("Loading…");
  const glue = await engine();
  glue.connect(server, null, `/${slug}`);
  await glue.start_public(`/${handle}/${slug}`, row ?? null);
  say("");
}

(mode === "app" ? app() : shared()).catch((e) => {
  say(String(e).includes("shared") ? String(e) : "Couldn't open this page: " + e);
});
