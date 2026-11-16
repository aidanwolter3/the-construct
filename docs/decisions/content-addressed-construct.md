# A construct is one hash: immutable elements, moving names, opt-in per pod

Superseded in part by [one-layout-system](one-layout-system.md): every pod is switched when it opens, the old system (its endpoints, tables, the switch and the revert) is gone, and clients use only the construct's paths.

Date: 2026-11-16. Related: [layouts-not-apps](layouts-not-apps.md), [layout-parts](layout-parts.md),
[plumbing-and-porcelain](plumbing-and-porcelain.md), [named-vocabulary](named-vocabulary.md),
[hold-changes-during-transitions](hold-changes-during-transitions.md), [source-adapters](source-adapters.md),
[rust-clients-on-the-engine](rust-clients-on-the-engine.md). Issues #295, #187 (narrowed), #296 (sharing).

Knowing whether a device shows a user's latest layouts meant comparing every layout's `(id, version)`: home's event
stream sent each layout's version on connect and the engine checked them one by one. What a version drew wasn't
fixed either: a component use without `version` followed the latest one, so the pod appended `ref_update` versions
to every dependent layout. The pod expanded components before serving, so the smallest thing a client could fetch
or check was a whole layout. Layouts and components were two concepts doing one job.

Considered:

- **A. Keep versions, add a digest of all `(id, version)` pairs.** One comparison, but a component edit still
  rewrites every dependent layout, and the client still fetches whole expanded layouts.
- **B. Content-address everything, switch every pod on deploy.** Simple to serve, but a deploy could break the
  developer's working setup, and every installed client at once.
- **C. Content-address everything, opt in per pod, with a dry run and a revert.** Taken.

Decision: **a pod's construct is a Merkle DAG of immutable elements rooted at home; names are moving pointers to
elements; a device compares one hash. A pod moves to it only when switched, after a dry run, and can be reverted.**

## Primitives

- **Object.** An immutable JSON value, identified by `e1-` and the lowercase, unpadded base32 SHA-256 of its
  canonical JSON (sorted keys, no whitespace: the same canonical form as layout files' append-only hash). The one
  kind today is an **element**: `{"kind": "element", "node", "params"?, "children"?, "title"?, "icon"?, "needs"}`.
  A layout and a component are both elements; "layout" is how one is used (presented or embedded), not a kind. A
  layout element's `node` is its `core://Page` (screens inside), a component's `node` its template. Entities stay
  out of the tree.
- **Name.** A mutable pointer to an object, with its history on the pod (`name_history`). Within a pod names carry
  no owner handle: a layout keeps its id as its name (`home`, `c-…`), a `pod:` component is `@me/<name>`, and what
  ships with the server is `@core/…`: a blueprint `@core/<blueprint>`, a part `@core/<blueprint>/<part>`, a server
  component `@core/<name>`. `@core/` names are moved by deploys (each pod re-points them to this build's files when
  it opens).
- **Reference.** `{"el": "e1-…", "name": "@core/card", "pin": false, "as"?: "<edge>"}` under `ref` in
  `core://Use`, `core://Layout` and `action.navigation.open`, replacing `component`, `core://Ref {app, part}` and
  `layout`. Authors may write `"ref": "@core/card"` (or the old spellings, which a write normalizes); the pod fills
  `el`. An unpinned reference is **re-sealed** when its name moves; a pinned one keeps its `el`. `follows` indexes
  name → the names whose elements reference it, so only those are re-sealed (replaces `ref_update`). A write that
  would make an element reach itself is refused (`cycle`).
- **The construct is the home element.** Its hash is the construct's hash. Every embed and forward link names its
  target by hash, so one hash fixes everything reachable from home.
- **Interface.** `needs: {params, feeds, sources, endpoints, data, jacks}`, derived by the pod on every write from
  the element (`$arg`/`$in` names, the feeds of home's it binds, data paths, endpoints, the sources' jacks). An
  author may add to it, never take away: what the pod derives is always included.
- **Binding.** Home binds the pod's real data: home's feeds are its `bind`, as before. A component use passes
  `args` (`bind` is accepted as its other name); inside, `$arg.<name>` and `$in.<name>` read them.

## Edges and the client-owned stack

- `core://Use` is transparent: no edge, repeatable. `core://Layout` and `action.navigation.open` are **edges**,
  keyed by the reference's `as`, else its name. Converted layouts keep their old ids as keys (`home`, `c-…`, and
  `as: "settings"` / `"inbox"` on blueprint links), so `local://nav`, notifications' `layout` and every screen
  route mean what they meant.
- The engine materializes each reachable element under its key (components expanded, each edge written back as
  `layout: <key>`), so the renderer's stack, screens and transitions are unchanged. With each new root it also gives
  the renderer the **edge graph** (`StateSnapshot::construct_json`, `Loader::edges`): key → the keys that element
  and the elements it embeds link to.
- A stack entry records its key and its presentation, transition, form values, scroll offsets and open screens; no
  hashes. When a new root arrives, each entry is checked against the one below it: at the first key the entry below
  no longer reaches, the stack is cut back there, each removed entry closing with its own transition.
- New intents **`action.navigation.pop_to {edge}`** (back to the nearest entry with that key) and
  **`action.navigation.pop_to_root`** (feature `navigate_pop_to`). Lateral links stay screens of one element.
- A new root is swapped in whole, and never while a finger is down, the keyboard is up or a transition runs (the
  rule [hold-changes-during-transitions](hold-changes-during-transitions.md) set): it waits for the first frame
  after.

## No versions on the device

On a switched pod the device keeps no versions: `local:layouts` and `local:all_layouts` are empty, `show_versions`,
`pin_version`, `follow_latest`, `hide_layout`, `unhide_layout` and the engine's pinned versions are not used, and
Settings is sealed without its Layouts section and versions screen. A device always draws the construct's current
hash. Names keep their history on the pod, for the agent: `restore_version` moves a name (home's too: the whole
construct) back to an earlier element.

## Sync and storage

- **`GET /v1/construct`** → `{root, generation}`: home's hash re-sealed for the client's features (each followed
  name at the newest of its history the client can draw; deterministic, cached per feature set and generation).
  On a pod that isn't switched: `409 {"error": "not_switched"}`, and the engine uses the old paths.
- **`POST /v1/objects {have, want}`** → `{objects: {hash: object}}`: the objects reachable from `want` (the root,
  then the stack's elements first) that `have` doesn't list. `GET /v1/objects/{hash}` answers one. Objects are
  answered only to the pod's own sessions; a hash grants nothing.
- **`GET /v1/construct/data/{hash}`** → `{entities, queries}`: the data of the home feeds the element needs, as
  `/v1/workspaces/<id>` composes it.
- **Events.** Home's stream sends `event: construct {root, generation}` on connect and whenever the root for that
  client changes. Data and notification events are unchanged.
- **Pod store.** `objects(hash, kind, bytes, features, size)`, `names(name, hash, updated_at)`,
  `name_history(name, seq, hash, at, author, note)`, `follows(name, follower)`, and the pod setting
  `construct.switched`. The tables are created by the first switch, so a pod that is never switched is never
  touched. Pod GC: after each write, objects no name's history reaches are deleted; each name keeps 500 entries.
- **Client store.** The engine keeps objects by hash in memory and in its store (`construct_objects`, issue #301),
  and each layout it draws in its cache as before (so the last screen shows at once on a start, offline too).
  Client GC keeps what the current and previous root reach (the stack's entries are reachable from the root, or
  closed), and the layouts opened by name.
- Components are expanded on the client: the expansion of definitions (parameters, `core://Children`,
  `core://IfArg`, `core://Fragment`, `$arg`) moved from `pod_core::components` and `pod_core::parts` into the crate
  `construct-elements`, which the pod and the engine both use. Elements are deduplicated and fetched one by one.

## Rollout: opt in per pod

- **Merging and deploying changes no pod.** Every pod stays on the layout system it has until it is switched; a pod
  that isn't switched answers every endpoint, event and MCP tool exactly as before.
- **New clients speak both.** The engine asks `GET /v1/construct` once a session opens; on `409 not_switched` (or
  an older pod's `404`) it uses `/v1/workspaces/…` exactly as before.
- **Switching** is a pod operation: MCP `switch_construct {dry_run: true}` and `server pods switch <id> --dry-run`
  convert the pod's live layouts and `pod:` components to elements and names in memory, then render every layout
  both ways with the pod's data (the preview path) and report each difference, changing nothing.
  `switch_construct {}` / `server pods switch <id>` write the new tables and set the flag, leaving the old tables
  as they are; `switch_construct {revert: true}` / `server pods switch <id> --revert` clears the flag, and the pod
  answers from the old tables again.
- **On a switched pod** the MCP tools write the new system (`create_layout`, `update_layout`, `restore_version`,
  `rename_layout`, `delete_layout`, the component tools, `expand`, previews). The old endpoints keep answering,
  from the new system, so a client installed before the switch still draws the construct: `/v1/workspaces/<id>`
  serves the name's element as the pod expands it, the versions API lists the name's history as versions 1…n, and
  home's stream announces those numbers. `/v1/custom-apps` writes, `set_hidden` and the device's
  `hide_layout`/`unhide_layout`/`delete_layout` are refused with `409 construct_switched`.
- **Revert** keeps what the switch wrote (the names keep their history), but edits made while switched stay in the
  new tables only: the old tables are as they were at the switch. Switching again converts the old tables again and
  moves each name to the new conversion, so the earlier elements stay in the names' history.
- Removing the old system (its endpoints, tables, aliases and features, and #187's grep gate) comes after the
  developer has switched and is happy: a follow-up issue.

## Consequences and limits

- Needs new desktop and Android builds for a switched pod to be drawn by the engine; installed builds keep working
  against a pod either way.
- `@core/` names follow this build: a deploy re-points them and re-seals their followers; a client that can't draw
  a blueprint's newest file gets the newest it can, by name history, as before.
- A launcher row bound to `local:layouts` with `$item.id` has no edge to resolve: the conversion writes such a list
  out as one row per live layout (its template with `$item.id`, `$item.title` and `$item.icon` filled in), and the
  dry run reports the difference (`$item.version` has nothing to show).
- `server pods switch` runs in another process: a running server re-reads whether a pod is switched every 5 s
  (MCP `switch_construct` takes effect at once).
- On a switched pod, Collections' `apply_change` can't edit layouts along with a collection (`409
  construct_switched`), and a collection change isn't checked against the construct's layouts as it is against the
  old ones.
- An installed client from before issue #126 (one that doesn't draw `page`) can't draw a switched pod's layouts: the
  construct serves plumbing only.
- A layout opened by name (a notification's, home not reaching it) is drawn off the tree: links inside it are opened
  by name too, and the stack never cuts it on a new root (home never reached it).
- A key draws the binding of the first use site that reaches it from home (breadth first): the same layout bound two
  ways needs two keys (`as`). An installed client older than `layout_bind` gets, by name history, the newest home it
  can draw, from before the bindings.
- MCP previews and `expand` of a layout that reads inputs show it unbound (its `$in.` names as written); its use
  sites are checked when they are written.
- Not yet: presence beyond what is shown (fetch order is the root, then what it reaches), and anchors of a scroll
  over a list nested in a row that is drawn again unchanged (it keeps its offset in points).

## Bindings, scroll anchors, kept objects, notification links (issue #301)

Added on 2026-11-16, for switched pods only (a pod that isn't switched answers as before):

- **`bind` on edges** (feature `layout_bind`). A `core://Layout` or `action.navigation.open` binds its target's
  inputs: `"bind": {"items": "collection:books"}`. A layout's own `$in.<name>` reads them (a list's `query_key`, a
  text); the engine materializes the target with them filled in (`construct_elements::materialize_with`; the same
  element bound two ways is two instances) and asks for its data with them (`GET /v1/construct/data/<id>?bind=`),
  so only the feeds the bound tree reads load, and only home's. Only home binds the pod's data; another layout's
  `bind` passes its own inputs on (`"$in.<name>"`, else `bind_outside_home`). Each element's inputs are checked at
  every use site on write (`unbound_input`), both ways: an edge that leaves one unbound, and a new input a use
  site doesn't bind yet. Home's feeds stay its `bind` (the switch and dry run report them as `home_bindings`).
- **Scroll anchors.** On a switched pod each vertical scroll of a stack entry keeps its first visible row (its
  entity URI) and how far the view is past that row's top; after the entry's data or element changes the offset
  is set so that row stays where it was, falling back to the nearest row still there (the next, else the one
  before), else the offset.
- **Kept objects.** The engine keeps the objects it fetched in its store (`construct_objects`, per app), reads them
  when a session is on the construct, and fetches nothing when the root it is given reaches only objects it has;
  client GC deletes from the store what the current and previous root (and layouts opened by name) don't reach.
  They are forgotten when the device leaves the pod.
- **Notification links.** A layout the root doesn't reach is looked up by name (`GET
  /v1/construct/names/<name>` → its element for the client's features), fetched and kept under its key, off the
  tree.
