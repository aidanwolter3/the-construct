# Claude edits custom apps through an MCP endpoint on each pod, with a host-issued, pod-scoped credential

Date: 2026-11-03. Related: D46, [custom-apps-and-data-sources](custom-apps-and-data-sources.md), issue #45 (P2).

Custom apps are written through `/v1/custom-apps` with the pod token. Users should be able to ask Claude for an app
instead ("a Kitchen app with recipes next to my feed"), without giving Claude the pod token (which opens every route:
the apps' accounts, sign-ins, devices, uploads) and without an AI key on the pod. The MCP spec (2025-06-18) has remote
servers use OAuth 2.1: protected-resource metadata (RFC 9728), authorization-server metadata (RFC 8414), dynamic
client registration (RFC 7591, a SHOULD; deprecated in the newer 2026-07-28 spec in favour of Client ID Metadata
Documents), PKCE, and the `resource` parameter (RFC 8707). Claude's custom connectors follow it.

Considered: an MCP server outside the pod, holding the pod token (it would hold the key to everything); API keys
typed into Claude (no standard way to enter one in Claude on the web); a per-pod OAuth server (the pod host already
owns credentials and routing, D46).

Decision:

- **Endpoint:** `POST /v1/mcp` in `pod_core::mcp`, streamable HTTP answered with one `application/json` response per
  request (notifications: 202; `GET`/`DELETE`: 405, no server stream, no sessions). Protocol versions 2025-06-18,
  2025-03-26, 2024-11-05 (the client's if known, else the newest). An `Origin` must be the request's host (DNS
  rebinding). It names no app.
- **Tools:** `list_apps`, `list_data_sources` (manifests with field docs and whether each sign-in is done; dev
  sources hidden), `describe_vocabulary` (a hand-kept table with a test that every `protocol::NODE_TYPES` entry is
  documented, plus intents and bindings), `get_layout` (any app, with history: built-ins are the examples),
  `sample_data` (a feed, or a declared data path with real values; at most 20 items per query, strings cut at 300
  characters), `validate_layout`, `create_app`, `update_layout` (a layout or an RFC 6902 patch against the latest,
  compare-and-append on `base_version`), `restore_version`, `rename_app`, `delete_app`, `set_hidden`. Writes call
  `pod_core::custom` with author `claude`; failures are tool results with `isError` and the same body the API gives
  (`at`, `allowed`), so Claude can fix them.
- **Samples act as the owner:** the pod's pinned (pod token) session. A feed runs with that session; a data path runs
  through the pod's own router with an internal header, `<per-process random secret>:<session hash>`, which nothing
  outside the process can make. Nothing else in MCP acts as the user.
- **OAuth on the host** (`pod_host::oauth`): `/.well-known/oauth-protected-resource[/v1/mcp]`,
  `/.well-known/oauth-authorization-server` (with `authorization_response_iss_parameter_supported`), `POST
  /oauth/register` (public clients; 1-5 redirect URIs, `https://…` or `http://localhost|127.0.0.1|[::1]`; 20 a
  hour per address), `GET /oauth/authorize` (a page asking for the pod code; CSP `form-action 'self' <client
  origin>`), `POST /oauth/authorize` (rate-limited like the code screen; redirects with a single-use code, 5 minutes,
  with `state` and `iss`), `POST /oauth/token` (PKCE S256 required; a `resource` must be this server's MCP
  endpoint). The base URL is `CONSTRUCT_PUBLIC_URL`, else the request's `Host` (`https` behind Fly).
- **The credential:** host.db migration 3 rebuilds `credentials` with `kind IN ('pod_token', 'mcp')` and a `label`,
  and adds `oauth_clients` and `oauth_codes`. The token endpoint creates an `mcp` credential (`cmcp_…`, only its hash
  stored). The host routes it only to its pod's `/v1/mcp`, with the bearer removed and `X-Construct-Credential: mcp`
  set (any client copy of that header, and of the internal header, is stripped from every request); on any other
  path it gets 403 `mcp_credential_scope`. A pod trusts the header only when a host serves it (`set_hosted`). The pod
  token also reaches `/v1/mcp`. `/v1/mcp` without a usable credential is 401 with `WWW-Authenticate: Bearer
  resource_metadata="…", scope="mcp"`, never the open pod.
- **Lifetime:** the access token doesn't expire and no refresh token is issued (clients must not assume one). It is
  revoked with `server pods revoke-mcp <name>`; `server pods rotate` replaces only the pod token, so Claude stays
  connected. `server pods mcp <name> [label]` makes one by hand for Claude Code.

Security: an MCP credential can't reach app routes, so a prompt injection in sampled data can at most write a
visible, versioned, reversible layout, which can only use what the app's sources declare. Sampled data is the user's
own, sent to their own Claude account.

Not done yet: Client ID Metadata Documents (the 2026-07-28 spec's preferred registration; the host would have to
fetch client URLs); short-lived tokens with refresh-token rotation (the spec's SHOULD); disconnecting Claude from the
app's Settings; a list of connected clients outside the CLI; Claude's connector callback URL could not be checked
against Anthropic's help page while writing this, so any registered HTTPS redirect is accepted.
