# Fetch fillers can render a page in a headless browser first

Date: 2026-11-14. Related: [collection-fillers](collection-fillers.md),
[web-and-agent-modules](web-and-agent-modules.md), [jacks](jacks.md), [ci-container-image](ci-container-image.md).

Two kinds of page gave a fetch filler nothing to read. Pages that build their content with JavaScript: Downtown
Boulder's events page is about 28 KB of HTML with no event in it; its listings (small venues such as "Trivia Night"
at Avanti, which its Google Calendar feeds leave out) load from a script. And product pages behind bot checks: a
plain fetch of an L.L.Bean product page answers a 2 KB check page with no Product JSON-LD, so the Style deck's
cards had no photos. `web-and-agent-modules` turned down a headless browser for a policy problem; here the problem
is rendering.

Considered: a rendering service as a jack (another account and its costs, and pages would leave the pod's host
policy); Playwright (a Node runtime in the image for what a few DevTools calls do); `chromium --dump-dom` (no wait
for a selector, no status of the page, no control of its requests).

Decision: a fetch filler with a `url` or `url_field` takes `render: true` or `{wait_for: <CSS selector>,
timeout_seconds: 5..=45}` (default 20 s), and so does `collections_preview`. The web module's `render` loads the
page (and the filler's `details` pages) in Chromium over the DevTools protocol, waits until the network has been
quiet for 0.5 s after the load event (or 2 s with requests still open, for long polls), or until `wait_for`
matches, and hands the rendered HTML to the extractors unchanged: `events`, `jsonld`, `selectors`, `llm` and the
rest read it as a fetched page. A `wait_for` that never matches is a failure, not an empty page.

- **Cost:** Chromium takes a few hundred MB while it renders, so the server runs one browser at a time (a render
  waits up to two minutes for the one before it), starts it per render with a throwaway profile and closes it
  after, and doesn't load images. A pod renders at most 100 pages a day. A rendered filler's `every_minutes` and
  `on_open_minutes` are at least 360. Staging's machine goes from 512 MB to 1 GB.
- **The host policy holds:** a render is one request to the page's host (spacing, daily cap). A 403 or 429 for the
  page, or a challenge page after it settles, is a block: the pod backs off the host (shared with plain fetches),
  `collections_status` reports the reason, and a `url_field` filler keeps the row's values. The pod never solves a
  captcha.
- **A realistic profile, no disguise beyond it:** the browser's own Chrome version in a desktop Linux user agent and
  client hints (not "HeadlessChrome"), a 1366×900 viewport, `Accept-Language: en-US`, and `navigator.webdriver`
  off. This passes L.L.Bean; J.Crew still answers 403 and is reported as blocked.
- **Public websites only:** every request the browser makes goes through a proxy of the pod's own on the loopback
  interface, which connects only to public addresses (`pod_core::net::is_public_ip`, checked on each answer) on
  ports 80 and 443 and stops a render after 40 MB. The browser resolves no names itself (`--host-resolver-rules`)
  and sends no UDP around the proxy (WebRTC). Chromium's sandbox is off (the server runs as root in its
  container); the proxy and the throwaway profile are what contain it.
- **No headers:** a rendered filler sends none (the browser would send them to every host the page loads from), so
  no secrets.
- **The server's browser:** `CONSTRUCT_BROWSER` names the binary (`off`: none); unset, the first Chromium on `PATH`.
  The pod image moves from distroless to `debian:trixie-slim` with Debian's `chromium` (the image grows by a few
  hundred MB and gains a shell). Without a browser, a rendered filler or preview says so. CI's image has Chromium,
  so the render tests run there; without one they skip.

No new layout vocabulary and no client build. Rendering is part of the `web` jack: temporary pods, with `web`
locked, don't render.

Not yet: a rendered page's validators (every render reads the page afresh; the body hash still skips unchanged
extraction); a server-wide daily cap across pods; keeping one browser warm between renders.
