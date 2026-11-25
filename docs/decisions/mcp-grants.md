# An MCP credential can carry a grant: the tools, rows and screens it may touch

Date: 2026-11-25. Related: [mcp-access-levels](mcp-access-levels.md), [edit-groups](edit-groups.md),
[anticipate](anticipate.md), [untrusted-tool-content](untrusted-tool-content.md).

Access levels say how far a connection reaches (read, edit, outward), and `mcp-access-levels` left per-tool grants as
"not done". An unattended job that reads strangers' mail (Anticipate's Sense run) or changes the owner's screens
(its Build run) needs less than `edit` gives: `edit` still allows `update_layout` on home, `restore_version`,
`set_theme`, bulk row deletes in any collection, `unplug_jack` and `mcp_disconnect`. A prompt-level rule ("only
write these rows") is one an injected email can talk the model out of.

Considered: a new access level per job (levels are ordered; these scopes aren't); a separate credential kind (a
second path through the host for one feature); per-collection grants only (the tools still reach everything else).

Decision: a credential may carry a **grant** on top of its level (`pod_core::grant::Grant`):

- `tools`: the only tools it may call (an allowlist); `tools/list` lists only those.
- `rows`: the only data sets (a source's names: collections, for Collections) whose rows it may write;
  `deletes` (or `no_delete: true`) the only ones it may delete in. The source checks them; the core knows no
  source.
- `protected_names`: layouts and components its edit groups may not change, nor a component one of them uses
  (`core://Use`, transitively). Layouts a protected one embeds or links to are their own names.

Stored on the credential (host.db migration 15, `credentials.grant_json`), set with `server pods mcp --access edit
--grant <file.json>`, shown in Settings → Connected apps (`grant_text`), and kept when the access changes there. The
host sends it beside the level as `x-construct-mcp-grant` (base64url of its JSON), strips that header from every
incoming request, and a pod trusts it only from its host with the `mcp` credential; one that doesn't parse allows
nothing. The pod checks the tool before it runs, then holds the grant in the call's task-local beside the level and
on every blocking thread the call uses (`Pod::blocking`), so the requests the call's tools make through the pod's own
router are held to it too. Collections checks every row its write transaction touched, whichever collection (an
action's target, the second collection of a two-collection edit, child rows), before the computes that follow,
and refuses the whole transaction (`grant_refuses`). `apply_edit_group` refuses protected names and groups that
aren't additive.

Not done: grants chosen on the OAuth consent page (the CLI makes them); a grant for row values (which fields).
