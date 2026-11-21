# Pods call remote MCP servers through an `mcp` jack, and Collections fills from their tools

Date: 2026-11-15. Related: [jacks](jacks.md), [collection-fillers](collection-fillers.md),
[web-and-agent-modules](web-and-agent-modules.md), [mcp-connector](mcp-connector.md).

Some of a user's data has no shareable link a filler can fetch, or has one that serves it badly. The first case:
Proton Calendar's share link is an iCalendar file of every event ever, oldest first, so a filler capped at 200
items never reaches the upcoming ones, and repeating events are not expanded. The user already runs an MCP server
for Proton (aidanwolter3/proton-mcp) whose `calendar_list_events` takes a date range and expands repeating
events; Claude reads it, but the pod could not.

Considered: raising the event cap and expanding `RRULE`s in the iCalendar reader (fixes one feed, not mail or
other servers); a scheduled Claude session copying rows into the pod (a model run per sync, rows no filler owns);
a Proton-specific adapter (the next server would need another).

Decision: the pod is an MCP **client** (`pod_core::mcp_servers`):

- **Jack:** `mcp`, a core switch, on by default and locked on temporary pods. Unplugged, no server is called
  and `mcp` fillers read `off`. `list_jacks` shows the connected servers under it.
- **Servers** are named entries (`mcp_servers` table, at most 20 a pod): an `https://` URL on a public host name and the default port
  (private hosts only with `CONSTRUCT_FILL_PRIVATE_HOSTS=1`) and its auth: `none`, `token` (a bearer the user
  gives once) or `oauth`. Secrets (token, access and refresh tokens, a client secret, the PKCE verifier) are
  sealed per server and field (`mcp_servers:<name>:<field>`) and never shown back.
- **OAuth** follows the MCP authorization spec: a 401's `resource_metadata` (else the well-known protected
  resource URLs, path first) names the authorization server; its RFC 8414 metadata (else OpenID configuration)
  gives the endpoints (S256 required when listed); the pod registers itself as a public client (RFC 7591,
  `token_endpoint_auth_method: none`, one registration per issuer and redirect URI) and answers a sign-in URL
  with PKCE, a random state (stored hashed, good for 30 minutes) and `resource` set to the server. The redirect
  URI is the pod's own public route, `<CONSTRUCT_PUBLIC_URL or the request's host><public prefix>/v1/mcp-servers/callback`,
  a GET without a bearer (a core public path); it trades the code for tokens and lists the tools. Access tokens
  refresh when within a minute of expiry or on a 401, one refresh at a time per pod (refresh tokens rotate); a
  refused refresh marks the server `needs_sign_in`, and `mcp_connect {name}` gives a new link.
- **Calls** use Streamable HTTP: each call opens its own session (`initialize`, `notifications/initialized`,
  the request, with `Mcp-Session-Id` and `MCP-Protocol-Version` once known), and reads the answer as JSON or as
  an event stream. No redirects are followed (a POST would turn into a GET; a token must not follow one off its
  host), a request has a minute, an answer at most 8 MB. The listed tools are stored capped (names of 128
  characters, longer ones skipped; descriptions of 600; a schema of 16 KB, else a bare object schema; 200 tools,
  1 MB in all), and a server's error text is cut to 300 characters. The public callback looks up the one server
  whose pending state matches, in SQL. A tool's value is its `structuredContent`, else its
  text parsed as JSON; `isError` is a failure with its text.
- **Claude's tools:** `mcp_connect`, `mcp_servers`, `mcp_call_tool` (runs a tool now, answer shortened; see
  below for which) and
  `mcp_disconnect`.
- **`mcp` fillers** (Collections): `{kind: "mcp", server, tool, arguments, items, zone, map}`. The answer is read
  as a JSON page (`items`, `map`, coercion, `upcoming`, `key`, modes and alerts as for any filler); an answer
  identical to the last writes nothing. Text arguments may hold `{today}` or `{today±N}` (days), resolved in the
  pod's zone at each fill, so a filler can ask for "the next six months". `collections_preview` with the filler
  calls the tool.

- **What the pod calls** (since issue #436, a security audit: `mcp_call_tool` could run any tool, so one prompt
  allowing it in the user's Claude client gave a prompt-injected Claude, or an injected filler, send and push
  rights on a mail or code server connected through the pod). Each listed tool keeps its `readOnlyHint` and
  `destructiveHint` (booleans only). A filler calls only tools marked `readOnlyHint: true` (unattended, on a
  schedule); `mcp_call_tool` calls those and the tools on the server's allow list, which `mcp_connect
  {allow_tools}` sets (replaced when given, `[]` clears it) and `mcp_servers` shows, with each tool's `read_only`,
  `destructive` and `callable`. A tool the server doesn't list is refused. Tools stored before hints were kept
  are listed again at their next call. The hints are the server's word, but the server is the one the user chose
  to connect; the rule stops a caller that was talked into a write, not a lying server.
- **Scopes:** a sign-in asks for none, unless the server's `WWW-Authenticate` names some (`scope`) or `mcp_connect
  {scopes}` gives them (kept for later sign-ins; `[]` asks for none). It used to ask for every scope in the
  authorization server's `scopes_supported`.
- **Sign-in host:** the server's metadata chooses the authorization endpoint, so `mcp_connect` answers
  `sign_in_host` with the link and tells Claude to name it to the user before they sign in.

MCP servers are the user's own services called with the user's credentials, so the web module's host policy
(spacing, daily caps, block detection) does not apply; fill intervals bound the calls. No client build: the
Settings jacks list shows `mcp` like the other switches, and nothing new is drawn.

Not yet: connecting a server from the app (Claude connects them; a sign-in link opens in any browser); servers
whose authorization server has no dynamic registration (a pre-registered client id); resources and prompts
(tools only); a server pushing changes (fills poll).

Since then: `{midnight}` / `{midnight±N}` arguments, the start of that day in the pod's zone with the offset in effect
on that date. A bare date is read as midnight UTC by some servers (proton-mcp), which cut a Denver window short by
seven hours; a fixed `-07:00` would be an hour off after daylight saving starts. Rows also gain `<datetime>_day`
(`Fri Nov 6`), so a layout can show the day and the time in their own columns.
