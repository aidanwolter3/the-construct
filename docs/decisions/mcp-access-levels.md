# MCP connections have an access level: read, edit or outward

Date: 2026-11-24. Related: [mcp-connector](mcp-connector.md), [connected-apps](connected-apps.md),
[agents-cap-v3](agents-cap-v3.md), [untrusted-tool-content](untrusted-tool-content.md), issues #438, #243, #439.

Every MCP credential ran with the owner's full authority, and the checks before outward actions were ones the model
could pass by itself: `share_publicly` and `publish` confirmed with a review id the model had just been given,
"ask before creating a schedule" was a sentence in a tool description, and rows Claude wrote counted as the owner's,
so Claude copying a filler's text into a queued task row turned it into a run (laundering the #243 origin check).
One prompt injection reaching the owner's Claude (a page, a comment, another MCP server's result, a publication)
could share data publicly, schedule unattended agent runs with repository access, or call write tools.

Considered: a pending-approval queue, where an outward call waits until a device taps Approve (safest per action,
but every outward step needs the phone, and it needs a new inbox flow); and scoped credentials, chosen once per
connection. The owner chose scoped credentials.

Decision:

- **Levels**, ordered: `read` (list, read, describe, preview), `edit` (also change the pod's own layouts,
  components, collections, rows, theme), `outward` (also act outside the pod or irreversibly: share or publish
  publicly, set a handle or calendar feed, schedule, change or link agent runs, add a filler or fetch a file or
  recipe from a new host, call or connect other MCP servers, plug jacks, set collection secrets, watch devices,
  delete a layout, component or collection). `pod_core::mcp::Access`.
- **Stored** on the credential: host.db migration 14 (`mcp_access`) adds `credentials.access` (existing MCP
  credentials get `edit`) and `oauth_codes.access`. A value this build doesn't know is `read`, never more.
- **Chosen** on the OAuth connect page (three choices, `edit` checked), carried by the authorization code to the
  credential; `server pods mcp --access` (default `edit`); the local testing key (`--mcp-key`, never on Fly) is
  `outward`. **Changed** in Settings → Connected apps (`POST /v1/connected-apps/access {id, access}`, layout
  `settings` 0012, existing vocabulary only): the host answers it for the pod's own sessions, so an MCP credential
  can't raise itself; raising to outward asks first.
- **Sent** by the host beside `x-construct-credential: mcp` as `x-construct-mcp-access`, which it strips from every
  incoming request; a pod trusts it only when hosted, and only with that credential (none: `read`). The pod token's
  own session is `outward`.
- **Checked** in `pod_core::mcp` before a tool runs: `list_`, `get_` and `describe_` tools read; the core places its
  own tools and each source its own (`Source::mcp_access`), some by their arguments (a filler in
  `define_collection`/`apply_change`, a `url` in `edit_rows`, the `op` of `agents_schedules`: `list` reads, `pause`
  edits). **Default-deny**: a tool nobody placed needs `outward`. A refusal is a tool error `access_too_low` with
  `needs` and `has`, telling Claude to tell the user. `tools/list` still lists every tool; `initialize` says the
  connection's level. A test pins every tool's level.
- **Provenance**: the call's level is kept in a task-local for the call, so requests its tools make through the
  pod's router see it too. Below `outward`, rows Collections writes for it (`add_row`, `edit_rows`, `run_action`,
  row writes through a layout's action in a preview) record the writer `claude` in `collections_field_writers`, like a
  model's: Agents' origin check refuses to start a run from such a row (the row says Claude wrote it), exports mark
  those values as untrusted, and text an action copies keeps the mark. Agents' reopen action refuses a call below
  `outward`. With `outward`, Claude's writes count as the owner's, as before.

Since then (2026-11-26, security audit): a per-row filler (`url_field`) fetched every row's URL whoever wrote it,
so an `edit` connection could reach any host by writing a row (`add_row`, `edit_rows`, `run_action`) and calling
`collections_refresh`, with the URL carrying what it read from the pod. A URL whose field Claude wrote below
`outward` (writer `claude`) is now fetched only from a host the filler already reaches: one a person's row names,
or one it filled a row from. Other rows wait, and `collections_status` says why. `query_field` searches go to the
pod's own search provider, a host it already reaches, so they are unchanged.

Not done: per-action approval on a device (the other option) and per-tool grants. A connection at `outward` is as
trusted as before this decision, so the owner should give it only to an app they trust with that.
