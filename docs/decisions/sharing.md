# Pods share elements by link through the host's shelf, and adopt them over their own data

Partly superseded by [adoption-capabilities](adoption-capabilities.md): what an adopted element may reach,
and how URL consent is kept.

Date: 2026-11-16. Related: D46, [content-addressed-construct](content-addressed-construct.md),
[collections](collections.md), [mcp-connector](mcp-connector.md), [source-adapters](source-adapters.md). Issue #296.

Every pod's layouts were its own: pods are separate databases (`pods/<id>/server.db`, D46) with no path between
them, and a pod had no owner handle, only an id and a token. A user couldn't give another user on the same server
a layout (or a whole construct) to use over their own data.

Considered:

- **Hash secrecy**: let any pod fetch any object whose hash it knows. Rejected: hashes of near-copies of public
  elements are computable, and an element names the owner's data (`collection:books`, a row's URI).
- **Share the owner's element as it is** and bind it on the adopter's side. Rejected: the element reads the
  owner's data literally, so a copy would read nothing (or, with a mapping layer, the wrong thing).
- **A shelf on the host, written only by publishing, with data references turned into inputs.** Taken.

Decision: **privacy is storage. A pod's objects never leave its database except when its owner publishes a name:
the pod copies the name's closure, turns each literal reference to the owner's data into an input, pins every
reference inside, and puts the copy on the host's shelf under `@<handle>/<slug>`. Another pod reads it only with
the publication's share link, and adopting copies it into the adopter's own store, bound to the adopter's data in
the adopter's own reference.**

## Handles and the shelf

- **Owner handles** (`set_handle`): `@<handle>`, 2–30 of `a-z0-9-` starting with a letter, unique on the server,
  kept in `host.db` (`handles`). A few are reserved (`core`, `me`, …). Inside a pod names still carry no handle
  (decision content-addressed-construct); publishing needs one. A handle can't change while live publications
  carry it.
- **The shelf** is `host.db`'s `publications(link, name, owner_pod, source, visibility, version, root, interface,
  approved, keep_shared, pending, created_at, updated_at, revoked_at)`, `shelf_objects(hash, bytes, features,
  size)` and `publication_objects(link, hash)` (migration `sharing`). One live publication per name; it keeps only
  its current version's objects (adopters keep their own copies). Objects no publication holds are deleted. A
  deleted pod's handle and publications go with it.
- Pods reach it through `pod_core::construct::share::Shelf`, which the host plugs into each pod it opens
  (`AppState::set_shelf`); a pod run without a host can't share (`no_shelf`).
- **Knowing a hash grants nothing.** `/v1/objects` still answers only the pod's own sessions from its own store;
  the shelf answers a publication's objects only to a pod holding its link, and only while it is live. The server
  operator can read every pod, as before.

## Publishing (`publish`)

- **Visibility `link` only**: unlisted, anyone on the server with the share link (`share-` and 32 base32
  characters, unguessable). `public` (listed) is in the model but refused (`visibility_not_available`) until
  there is moderation and limits.
- **The copy.** The pod walks the name's closure (what it embeds, links to and uses), copying each element per
  use site with that site's bindings applied, so every data reference is literal, then:
  - each source turns its literal references into inputs (`Source::share_inputs`). Collections makes each
    collection an input (`collection:$in.<c>`, `collections://$in.<c>/…`, `/v1/collections/$in.<c>/…`) and each
    field it reads or writes a **field input** (`$item.$in.<c>__<f>`, a mutation's `field`, a create's entity and
    payload keys), so an adopter's binding can rename fields; system fields keep their names;
  - a reference to one entity (`${collections://books/7#title}`, any `${<scheme>://…}` but `local`/`core`) is
    refused at its pointer (`literal_data_reference`), and so is a component that names data itself (it must
    take it as an argument);
  - every reference inside is pinned (names inside are provenance only; adopters follow the published name);
  - edges pass their target's inputs on (`"bind": {"<i>": "$in.<i>"}`), so the root's inputs are the
    publication's interface; the root's own bindings (home's feeds, its use site's `bind`) are dropped.
- **Review.** Without `confirm` nothing is shared: the answer lists every element, the interface and each
  **flag**, literal text a person reads (titles, labels, `value_expr` once expressions are taken out) and every
  URL, so personal words ("Dinner party · Sunday") or a private URL are seen before they go. Every literal
  string the owner's home binds into the root (`binding` flags) and every string of an input's suggested schema
  (field defaults, options, titles: `schema` flags) is flagged too, wherever it sits. The owner may leave
  elements out (`leave_out`: the embed goes, or the node whose intent opened it). `confirm: <review_id>` (a hash
  of the copy's root, name and visibility) publishes exactly what was reviewed; anything changed since asks
  again.
- **Keep it shared** (`keep_shared`): after each write that moves the published name, the pod copies it again
  and publishes the new version without asking while its flags are all among the approved ones; new text or URLs
  (or a copy that can't be made) wait as `pending_review` in `list_publications`.
- **Derived interface**: each input with its source, kind, title, the source's feed (`$in.<c>`), the fields read
  and written with the owner's types, and a **suggested schema** (`{"tool": "define_collection", "args"}`: the
  definition without data, fillers, alerts, reminders, actions or computed values); value inputs; the root's
  params; its derived `needs` (sources, jacks, endpoints, data paths); what was left out; the author's `notes`.
- **Revoking** (`revoke_publication`) stops the link and new versions and takes the objects off the shelf.
  Adopted copies keep drawing (the review's instructions tell the agent to say so up front). Publishing the name
  again makes a new link.

## Adopting

- `describe_publication {link}` shows the interface; nothing is copied.
- `adopt {link, bindings, mode}` plans a binding per input: an existing collection (`{"collection", "fields":
  {<shared>: <mine>}}`), a new empty one from the suggested schema (`{"new": <name>}`, made with the source's own
  tool), or a value. Without full bindings (or with `dry_run`) it changes nothing and answers the problems (a
  missing field, a type mismatch) and a **proposal** per input (the collection with the most matching fields, a
  mapping by type for the rest, or a new one). With them it checks the root bound that way against the adopter's
  sources (the collections' own checks run), stores the snapshot under `@<handle>/<slug>` (author `adopt`), adds
  the bound data sets to home's feeds, and answers the reference to put in home: `use` (`core://Layout`) or
  `open`, each with `"as": "s-<handle>-<slug>"` and the bindings in its `bind`. The shared objects keep their
  hashes; the bindings live in the adopter's reference.
- **Modes**: `follow` (default), `pin` (this version only) or `fork` (a layout of the adopter's own, `c-…`,
  without updates). The adopter's pod records follows and pins in `adoptions(name, link, publication, version,
  mode, bind, …)`, created on first use.
- Every `adopt` and `update_adoption` answer lists the publication's flags (literal text and URLs), and a URL that
  would send the adopter's data to another server waits for the adopter's consent (decision
  [`adopt-shows-urls`](adopt-shows-urls.md)).
- **Following** advances when the adopter's agent asks: `list_adoptions` shows each adoption's version, the newest
  and whether it was revoked; `update_adoption` moves the name when the new version's interface still fits the
  saved bindings, and otherwise answers what to bind ("version 5 reads `chores__quantity`": a new field, or a
  reference whose `bind` lacks it). A pod never writes into another pod: nothing advances by itself.
- No new vocabulary and no client build: share links work through the agent (MCP). A pod draws an adopted
  element like any other.

## Rollout and limits

- Every pod is on the construct (decision `one-layout-system`), so every pod of a hosted server can share; a pod
  run without a host answers `no_shelf`.
- Not yet: `public` (listed) publications and browsing them (needs moderation and limits); opening a share link
  on the device (needs vocabulary and a client build); **slots** an adopter fills (an element-valued input, an
  engine change): a left-out element is removed instead; following in the background.
- A field mapping reaches fields read in a list over the collection (`$item.<f>`, its `where`, `group_by` and
  `filter`), a count's conditions, and mutations of its rows. Fields read inside a component the list uses (passed
  as `$arg`) aren't rewritten; the adopt check reports them if the adopter's names differ.
- Snapshots pin `@core/` elements too: an adopted copy draws the blueprint parts of the build it was published
  from until a new version is adopted.
- Types are compared by name only (`type_mismatch` is reported, not converted).
