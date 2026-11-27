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

Since then (2026-11-26, security audit): the protected names hold on every write of a group's names, not only its
first apply. Under a grant, a redo (`apply_edit_group` on an undone group) is held to apply's checks (safe, nothing
protected); `revert_edit_group` (by id, or every group `since`, refused whole) and `restore_checkpoint` refuse
when they would move a protected name; and Keep (`apply_edit_group` on a trial) is the owner's alone
(`owner_keeps`), so a scoped job can't turn off a trial's next-morning undo. Undoing an unprotected trial stays
allowed: Build undoes its unkept trials each morning.

Since then (2026-11-26, background agent security): a grant limits reads too. A job that reads the owner's
collections and is steered by what it reads must not read text strangers wrote there (filler and web values,
model-written fields), and a write-only limit let it read every collection.

- `reads`: the collections whose rows its tools answer. The Collections source refuses `describe_collection`,
  `export_collection`, `collections_status`, `edit_rows`, `add_row` and `run_action` on any other (those in `rows`
  are readable too); with a `reads` list the core refuses `sample_data`, which reads any source.
- `strip_untrusted`: after the tool runs, the core walks its answer (the structured content and every JSON text
  part) and turns each `untrusted` envelope (decision `untrusted-tool-content`) into `withheld: written by others`.
  A number, true or false, an empty value, or a date or time (only digits, separators and a zone) stays, out of
  the envelope, so the job still sees when things are. `keep_from` lists writers whose envelopes stay as they are
  (`claude`: the owner's own Claude sessions and jobs, so Build's journeys and Sense's need titles still read).
  Considered: dropping the wrapped field (the job couldn't tell empty from withheld); stripping in each source
  (every future source would have to remember).
- Previews and journey walks still draw screens with whatever they bind: `reads` holds the tools that answer rows,
  not the screens a job is shaping, and a preview is the pod's own picture of the owner's screen.

Since then (2026-11-26, background agent security, step 3): a grant also closes the ways out that skip a job's
sandbox, the pod's own fetches and the screens others see. Each call widens the grant, read fresh
(`grant::for_call`), so no grant file has to list them and none can leave them out:

- **Shared screens are protected.** A live publication's name and a live public link's layout join
  `protected_names` (with every component they use), so a job can't pull the owner's rows onto a page others read.
- **Shared rows aren't written.** A collection a calendar feed reads, or one whose rows a public link that publishes
  new rows shows, is written by no grant (`shared_rows`), so a job can't put text in front of subscribers.
- **Hosts are the screens' hosts.** The web hosts the pod's live screens use (every address in them, components
  expanded) are the only ones a granted call's rows may name (a new address on another host is refused; one the row
  had stays), and the only ones the pod fetches from for the call: `Hosts::admit` (fillers, the agent's fetch,
  search, the page renderer), a web client's requests and their redirects. The limit is a task-local, carried into
  the fillers, research, computed fields and taste fit checks the call starts (a queued fit check keeps the limit of
  the call that queued it); a filler's later scheduled run isn't the call's.
- **The host check runs at stage.** The edit groups' check now reads every address in a value, whichever field
  holds it (an image's `url`, a link's, a Markdown link in a text), and under a grant `stage_edit_group` refuses a
  group that reaches a host its screen didn't (`new_host`), so a staged group is never previewed or walked with it.

Since then (2026-11-26, step 4): **cards open the owner's screens.** A row field a live screen opens as an
address (`target_uri` bound to `$item.<field>`) is written by a granted call only with an address one of the
pod's screens' routes matches, as clients match it (`Grant::nav`): a card can't be pointed at a web page or a
screen the pod doesn't have. Clients already open only routes their screens declare; this makes the limit the
server's too, and refuses the write instead of leaving a dead tap.

Considered: listing shared screens in each grant file (they change as the owner shares); a fetch limit on every
outbound client (push and the pod's own connectors aren't steered by what a job writes).

Not done: grants chosen on the OAuth consent page (the CLI makes them); a grant for row values (which fields).

Since then (2026-11-26, step 5): every call a credential makes is logged by the pod under the credential's name,
with whether its grant or access refused it, a granted credential refused 3 times in an hour raises a Runner
warning, and the owner can undo a morning's background work in one step (decision [`mcp-call-log`](mcp-call-log.md)).
