# Every request to a host a user picked goes through one client that checks the URL and caps the body

Date: 2026-11-21. Related: D45, D52, [jacks](jacks.md), [web-and-agent-modules](web-and-agent-modules.md). Issue #447.

The pod calls hosts its users pick: a typed Pixelfed instance, a device's push endpoint, an MCP server, an agent
runner, a filler's page, an image for the web client. `pod_core::net::public_builder` already kept those off the
pod's own network (no proxy, a resolver that keeps only public addresses, redirects re-checked on every hop), but it
was opt-in: each call site had to pick it, and separately check the URL's name, which is the only defence against an
IP literal (hyper skips the resolver for one). Pixelfed kept its own builders. Body caps lived in each caller too,
and Pixelfed's API answers had none, so an instance typed under `PIXELFED_ALLOWED_INSTANCES=*` could stream hundreds
of MB per call into the VM that holds every pod.

Decision: **`pod_core::net::OutboundClient` is the only way to build a client for such a host.**

- `OutboundClient::builder(timeout)` makes one; `.private(true)` lets it reach any `http(s)` host (hosts the
  operator configured, tests), `.web()` lets a public name use `http://` and any port (web pages), and otherwise a
  URL must be `https://` on the default port of a public host name. `.no_redirects()` and `.redirect_if(..)` narrow
  redirects; a public client follows one only to a public `https://` name, at most 5 hops.
- `get`/`post`/`head` check the URL first (scheme, public name, no IP literal, port); a refused URL fails at
  `send`, before any connection.
- Bodies are read through one cap: 4 MiB unless the client (`.body_cap`) or the request or answer says otherwise;
  `bytes`, `text`, `json`, `chunk` and `into_stream` all stop at it. Pixelfed's API answers are capped at 4 MiB.
- `clippy.toml` refuses `reqwest::Client::builder`, `Client::new`, `ClientBuilder::new` and `reqwest::get` (and
  `net::fixed_host_builder`, for fixed hosts the operator configured, such as the Messages API) outside `net.rs`. A
  call site that builds one for a fixed host, its own pod or a test allows the lint there, saying why.

Not yet: Chromium's rendered fetches (`web_extract::render`) go through their own proxy and checks, not this client.
