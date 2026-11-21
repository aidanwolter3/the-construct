# Connected apps: MCP credentials are listed and revoked in Settings, end when idle, and OAuth stops trusting self-chosen names

Date: 2026-11-21. Related: [mcp-connector](mcp-connector.md), [oauth-preregistered-client](oauth-preregistered-client.md),
[jacks-in-settings](jacks-in-settings.md), issue #440 (security audit).

Anyone can register an OAuth client with any name ("Claude") and any `https://` redirect. The connect page showed the
redirect's origin with its userinfo (`https://claude.ai@evil.example/cb` read as `claude.ai@evil.example`), a victim
typing the pod code gave the attacker an MCP credential that never expired, rotating the pod code deliberately kept
it, and only the admin CLI could revoke (all of them at once). A self-registered client could also use
`/oauth/authorize` error redirects as an open redirect from the server's origin.

Considered: short-lived access tokens with refresh-token rotation (the spec's SHOULD; more state and a second secret
per connection, and MCP clients must not assume refresh tokens, so a connection could silently die); an expiry from
issue (Claude would need the pod code again on a fixed schedule, used or not); a device-data (`local:`) list, which
would need an engine change.

Decision:

- **Redirect URIs** are parsed: the authority may not hold `@` or `%`, the URI no `\`, whitespace or control
  characters, the host only letters, digits, `-` and `.` (or `[::1]`), and plain `http` only to the local machine.
- **The connect page** shows the host the code goes to in large type, before the client's self-chosen name, and a
  red warning when it is not a known MCP client host: `claude.ai`, `claude.com` (or a subdomain), or the local
  machine. The pre-registered client's hosts are trusted too (the operator set them).
- **Request errors** (bad `response_type`, PKCE, `resource`) redirect back only for those trusted clients; any other
  client gets an error page here, so the server is no open redirect.
- **Idle expiry:** host.db migration 11 (`mcp_credential_use`) adds `credentials.last_used_at` (existing MCP
  credentials count as used at the migration) and `redirect_host`. A use records the time at most once an hour; an MCP
  credential unused for 90 days (`MCP_IDLE_SECS`) is no credential and the sweeper deletes it. No refresh token.
- **Strict kinds and web expiry:** a `credentials.kind` this build doesn't know is no credential (it never parses as a
  pod token). Migration 12 (`web_credential_expiry`) adds `credentials.expires_at`: a browser's `web` credential stops
  working at its session's end (`CONSTRUCT_SESSION_TTL_DAYS`; a web row with no expiry is refused), which also bounds
  `/web/image`, and the sweeper deletes expired rows.
- **Settings → Connected apps** (layout `settings` 0010, no new vocabulary): a screen over
  `GET /v1/connected-apps/screen[?id=]` (entities `connected_apps://<id>`, query `connected_apps`; `id` is the first 16
  hex characters of the credential's hash) with a Disconnect per app, an `action.confirm` then
  `action.mutation.optimistic` (`connected` → false, which hides the row) to `POST /v1/connected-apps/revoke {id}`.
  The credentials live in host.db, so the **host answers these two paths itself**, for the pod's own sessions (pod
  code, a browser it opened, the open pod), before forwarding; an MCP credential still gets 403 there. The Settings
  manifest declares them like any data path.
- **Rotation** offers to disconnect: `server pods rotate <name> --revoke-mcp`; without it the CLI says how many apps
  stay connected and how to disconnect them. The connect page now points at Settings instead of the CLI.

No client build: the layout uses existing nodes and intents. Pods with their own copy of Settings keep their copy and
don't get the section.

Not yet: refresh tokens; Client ID Metadata Documents; the pod's MCP tools don't list connections (Claude shouldn't
revoke itself); browser (`web`) sessions aren't in the list.
