# Elements have a role and a scope; reuse is taught, hinted and made cheap

Status: **a proposal on a branch; nothing here is built, and this file is not meant to merge as it stands.**
[README](README.md) says a decision lands with the change it records. This file is the design the developer asked
for ahead of the work; once the direction is agreed, the list under [Features](#features) becomes a tracking issue,
each feature lands with its own decision file, and this file is deleted. The features marked "client build" are
not started without the developer's go-ahead (`CLAUDE.md`, "Ask before a change that needs a client build").

Date: 2026-11-20. Reviewed twice before this revision: every statement about today's code was checked against the
source, and the design was read for gaps; see [Review](#review).

Related: [content-addressed-construct](content-addressed-construct.md),
[one-layout-system](one-layout-system.md), [plumbing-and-porcelain](plumbing-and-porcelain.md),
[component-json-params](component-json-params.md), [layout-features](layout-features.md),
[blueprints-and-home](blueprints-and-home.md), [layouts-not-apps](layouts-not-apps.md),
[trace-from-home](trace-from-home.md), [sharing](sharing.md), [adopt-shows-urls](adopt-shows-urls.md),
[custom-theme-tokens](custom-theme-tokens.md), [remembered-form-values](remembered-form-values.md),
[one-button-node](one-button-node.md), [mcp-connector](mcp-connector.md).

## What was measured

An audit of what the server ships and of the developer's own pod. Shipped files and the pod's `home` were counted
by script; the pod's other 35 layouts and its 44 components were read in full and tallied by hand (±15%).

- **Shipped**: 37 components (34 in `pod-core`, 3 in `source-pixelfed`) and 7 blueprints, one of them the dev-only
  `pixelfed_sample_no_login` (542 nodes in their newest files). 69 nodes (13%) are `core://Use`. 17 components are
  used by no blueprint and no other component: `button`, `chevron_card`, `date_block`, `divided_item`,
  `filled_card`, `footer_bar`, `grouped_card`, `labeled_text`, `muted_note`, `muted_text`, `scroll_page`,
  `section_label`, `stat`, `text_link`, `title_meta`, `two_pane`, and `pixelfed_sign_in` (used from Rust).
  `workspace` and `screen` are used only through the conversion of the blueprints' `core://Workspace` roots.
- **The pod**: 36 layouts and 44 `@me/` components. `home` is 672 authored nodes (97 KB); 52 of them (8%) are
  `core://Use`. About 15 of the 44 components are aliases of `@core/` ones, duplicates of each other, or used
  nowhere. Five layouts look copied from built-in ones (home, inbox, events, pixelfed, a reworked settings).
- **Why nodes are written by hand**, most costly first:
  1. A template can't repeat over a list argument, and choosing by a key takes one `core://IfArg` `equals` per case.
     The directives are `$arg`/`$in`, `core://Children` (positional, `at`), `core://IfArg` and `core://Fragment`
     (`construct-elements/src/materialize.rs`). So a tab bar, a pane switcher and a chip grid are unrolled: the
     dock's tab mutation is written 30 times, the pane cases 16, time-slot chips 98.
  2. Children are positional and `@core/icon_row` takes none, so a row with a checkbox, a tag or a swatch is built
     by hand.
  3. A component with several states repeats its whole body per state.
  4. The theme carries colours only (`#RRGGBB`, `porcelain.rs`), so a restyle of corners or paddings needs parallel
     components and copied layouts.
  5. The current tab and similar device state live in collection rows addressed by a literal row id (one row is
     bound 44 times in `home`). `core://ChoiceChips` `remember` is the only value a device is meant to keep;
     `action.local.set` lasts until its entity next loads from the pod.
  6. Agents don't find what exists: `@core/muted_text` is used about 14 times in the pod and built by hand about
     150 times.

## Questions this answers

### 1. Is "blueprint" still a concept?

Not a kind of element. A blueprint is stored like every element (`kind: "element"`), named `@core/<id>`, with a
`core://Page` as its node. What sets it apart is its **role** (a layout: reached through an edge, with its own
screens) and that a source declares data beside it (`BlueprintInfo`: `id`, `title`, `icon`, `dev`, `auth`,
`feeds`; notification channels name it through `Source::notification_channels`).

It is more than a word in code, though. About ten paths key on the catalog of blueprints: a bare key resolves to
`@core/<id>`; `HOME_BLUEPRINT` seeds a new pod's home; a blueprint binds its own feeds wherever it is drawn;
sign-in gating; a notification's owning layout; the `copy_of` title heuristic; `list_layouts`' `blueprints` and
`list_components`' `kind: "blueprint"`; `list_parts` and the `dev` flag; a component may not take a blueprint's
id; the public copy. Every one of them is about "a layout a source ships", and none needs a third kind.

The word has a cost: it names a kind beside "layout" and "component" that isn't one, and it carries the old habit
"start from a blueprint, copy it to change it". Copies are what the audit found stale: the pod's Pixelfed copy
lacks the blueprint's Retry and Discard row, and the Events blueprint lacks what the copy added.

Considered:

- **Keep "blueprint".** No churn; agents keep learning a kind that doesn't exist.
- **"Public element".** Rejected: `public` already means a publication's visibility ([sharing](sharing.md)) and
  public web links.
- **Describe an element by its role and its scope.** Taken.

Decision: **an element has a role and a scope.**

| | Meaning | Spelled |
|---|---|---|
| Role **layout** | A screen: its node is a `core://Page`; reached through an edge (`core://Layout`, `action.navigation.open`) | "layout" |
| Role **component** | A template expanded in place by `core://Use`; no edge | "component" |
| Scope **core** | Ships with the server (the core's or a source's), moved by deploys | `@core/<name>`: "a core layout", "a core component" |
| Scope **own** | The pod's, including an adoption the user forked | a layout's id, `@me/<name>` |
| Scope **shared** | Another pod's publication, adopted here | `@<handle>/<slug>` |

"Part" stays: `@core/<layout>/<part>`.

The rename, three PRs in the order they pay:

1. **What agents read.** MCP instructions, tool descriptions and answers say "core layout". `list_layouts` answers
   `core_layouts` and also `blueprints` until the PR that removes it (named in the first PR's follow-up issue);
   `list_components` rows carry `role` and `scope` in place of `kind`. `copy_of` stays.
2. **Docs**, including the names inside old decisions, which is README's rename exception, with a row in its
   Renamed table.
3. **Code** (521 occurrences in `.rs` files under `crates/`, 614 in all files there: `BlueprintInfo`,
   `Source::blueprints`, `HOME_BLUEPRINT`, …), mechanical, then `scripts/check_old_names.sh` gains the word outside
   migration code. The script matches case-sensitively, so it takes both spellings.

No wire change (the wire types name blueprints only in comments) and no client build. Connected agents see the
new answers after "Refresh tools list".

### 2. Is "layout" still used, or is everything an "element"?

Both, for different things. **Element** is the stored object: immutable, named by its hash, the unit a device
fetches and keeps. **Layout** and **component** are the roles an element is used in, and the role decides
behaviour: a layout is an edge the navigation stack keys, a component is transparent. Prose, tool names
(`create_layout`, `define_component`) and code keep the role words; "element" is used where storage, hashes, sync
or sharing are meant.

Rejected: calling everything an element. It hides the one distinction an author must get right and renames the
whole tool surface.

### 3. Does the server send layouts inflated?

No. A device asks `GET /v1/construct` for the root hash, then `POST /v1/objects {have, want}` for the objects it
lacks. The engine keeps them by hash (`construct_objects`) and expands components itself with `construct-elements`,
the crate the pod also uses. A component used by twenty layouts is fetched once. For an older client the pod
re-seals rather than expands: it picks, per name, the newest history entry that client can draw. Components are
expanded on the server only for MCP answers (`expand`, `get_layout {expanded}`, `validate_layout`), previews, and
public web links.

So devices already cache building blocks. Two properties limit how much that helps, and both show in the audit:

- **The unit is the element.** Nodes written inline are part of their layout's one object. `home` is one 97 KB
  object; nothing in it is shared.
- **A change moves every element above it.** An unpinned reference holds its target's hash, at all three sites
  (use, embed, link). When an element changes, `reseal_followers` writes a new object for every element that
  references it, transitively, up to `home`. A component edit therefore re-seals every layout that uses it and
  everything above those. Each re-seal is a new object a device fetches whole, and an entry in the name's history,
  which keeps 500. `home` is at entry 682 and its 20 newest are all re-seals: devices fetched a new 97 KB object
  each time though no node of `home` changed, and real edits are being pushed out of what `restore_version` can
  reach.

Considered:

- **A. Keep the model and make elements small.** Hubs hold structure only, repeats become components. Fewer bytes
  per re-seal, but the same number of new objects, and more components mean a wider fan-out per component edit. It
  depends on every agent's discipline.
- **B. References by name only.** Ends re-sealing but gives up "one hash fixes the construct", which sync, the
  stack's cut-back and publishing rely on.
- **C. A root manifest.** The construct's hash is the hash of a manifest `{name → hash}` for every name home
  reaches; an unpinned reference carries its name and no hash; a pinned one keeps its hash. One hash still fixes
  the construct. An edit costs the one changed element and a new manifest (about 80 bytes a name: under 4 KB for
  this pod). Re-sealing, its history entries and `follows`' rewriting go away; sealing for a client's features
  becomes choosing that client's manifest. It changes the wire (`/v1/construct` answers the manifest), the engine's
  loader and GC, and publishing (which pins the closure from the manifest): a client build.
- **D. Send a re-sealed element as a diff.** Saves bytes on the wire only; two encodings; history still fills.

Decision: **C is the fix, as its own feature with its own decision. Until it lands, two pod-only changes:** a
name's history keeps 500 authored entries and re-seals don't count toward them (consecutive re-seals are stored as
one), and the pod reports what an edit costs so it can be seen: `list_layouts` adds each element's `bytes`, and a
write's answer adds `refetch: {objects, bytes}`, what a device with the previous construct must fetch. A applies
either way, because small elements are also what makes reuse possible; it stops being the only defence.

The number to track is **bytes a device fetches per edit**. Baseline on this pod: about 97 KB or more for any
edit, since `home` always re-seals.

### 4. Does an agent get context when it connects, and can it be tuned for reuse?

Yes. The MCP `initialize` answer carries `instructions`, built by `pod_core::mcp::instructions`: the constant
`INSTRUCTIONS` (5,293 characters) and a paragraph from each source that has one (collections, events, agents).

Three findings:

- **A client can cut it.** The sessions that ran this audit and its reviews each received exactly the first 4,096
  characters. Lost: how a `core://Page` draws its header, theme tokens, `action.confirm`, jacks, sharing, and every
  source's paragraph.
- **Reuse comes late and thin.** Two sentences: use a blueprint, part or component by reference to keep it
  current; and "Prefer a component over repeating nodes…", 3,500 characters in. Nothing says to look at what
  exists before writing nodes, and nothing at the point of writing says so either.
- **An agent can't learn what other people published.** Publications are reached by share link only; the listed
  shelf is refused today. Until it exists, the core set is the only public catalogue.

Decision: **short instructions that put reuse first, detail on demand, hints where the writing happens, and one
tool that answers "what already exists".**

- **Instructions under 2,000 characters**, held by a test. In this order: what the pod is; the build procedure;
  "change what is drawn, not what it was copied from"; the topic names. The rest moves to a read-only tool
  `guide {topic}` (`navigation`, `pages`, `theme`, `actions`, `jacks`, `sharing`, one per source, and `all`: the
  text as it is today). A source's `mcp_instructions` becomes its topic. Tool descriptions keep the rule that
  applies to that tool.
- **The build procedure**, as the instructions state it:
  1. Find what is there: `list_layouts`, then `find_elements`.
  2. Use a core or adopted layout whole when one fits, bound to the user's data.
  3. Otherwise compose from components: a scaffold, sections, rows. Write plumbing only for what no component
     draws.
  4. A piece that appears twice is a component: `define_component` before the second copy.
  5. Look at it (`render_layout_preview`) and act on the write's hints.
- **`find_elements {query?, role?, scope?, category?}`** in place of reading all of `list_components` (one flat
  list, 52 KB on this pod). It answers name, role, scope, category, a `use_when` line and parameters, ranked by
  the query, over core, own and adopted elements. `scope: "shelf"` is added with the listed shelf and answers
  publications by name, summary and interface. `category` (`primitive`, `row`, `container`, `scaffold`, `pattern`)
  and `use_when` for core elements live in a sidecar file per crate (`components/catalog.json`), not in the
  component files, which are append-only and whose every change re-seals every pod.
- **Hints on every layout and component write**, added to the `warnings` a write already answers
  (`validate::fixed_colours`, `validate::button_faces`, attached by `mcp::with_advice`):
  - a subtree that is what a core or own component expands to, with the component and the arguments to use. The
    match is exact: the component's parameters are wildcards, every other literal must be equal. Templates are
    indexed by a shape hash, so a write costs one pass over its nodes;
  - a subtree repeated three or more times in the write, equal but for values in the same positions;
  - a literal row id in a binding (`collections://<collection>/<row>#<field>`);
  - `refetch` when it passes a threshold set from the first month's measurements.
  Hints are advice: the write succeeds. At most 10 per write, largest saving first. They are computed after the
  write's transaction, with a time budget; past it the answer says hints were skipped. Before it ships, the matcher
  is run over the audited pod and its wrong suggestions counted; it ships only with none on that pod.
- **`note_gap {what}`**: when nothing fitted and the agent wrote plumbing, it says what it needed in a line. The
  pod keeps the lines (no user data: the agent is told to name the shape, not the content) and the operator reads
  them with `server pods gaps`. This is how the core set learns what to add next without anyone upstreaming.
- **Measures**, on a fixed set of five prompts run against a fresh temporary pod on staging, recorded before the
  change and in each PR that claims to help: authored nodes per screen; components with two or more uses (so
  single-use wrappers and aliases don't count); bytes fetched per edit; hints left after the agent says it is
  done. The share of `core://Use` nodes is not a measure: aliases raise it. A test in `crates/e2e` drives the MCP
  tools with scripted calls and asserts that each hint fires; it can't judge a model's choices, which is why the
  prompts are run by hand.

Considered: one long instruction (already cut); guidance in tool descriptions only (an agent that never lists
components never reads it); refusing writes that repeat nodes (blocks honest one-offs and sets agents against the
validator).

## Features

Each row is one feature and one PR. "Needs" is what must be out first.

| Feature | Needs | Client build |
|---|---|---|
| Words for agents (question 1, step 1) | | No |
| Words in docs; words in code (steps 2 and 3) | Words for agents | No |
| Short instructions and `guide` | | No |
| `find_elements` and the catalog sidecar | | No |
| Write hints and `note_gap` | | No |
| History without re-seals; `bytes` and `refetch` | | No |
| Root manifest (question 3, C) | | **Yes** |
| Core set: new components | | No |
| Core layouts on the core set, drawing the same | Core set | No |
| Core layouts catch up: events, inbox; server health; task queue (one PR each) | Core layouts on the core set | No |
| Template directives: `core://Each`, `core://Match`, named slots | | **Yes** |
| Tones | Core set | No |
| Theme scale | | **Yes** |
| Kept device values | | **Yes** |
| `dates` feed | | No |
| `tab_shell` | Template directives, kept device values | No more than theirs |
| Core kits | Core set | No |
| Shared kits and the listed shelf | Core kits | No |

### The core set

Categories are metadata for `find_elements`, not new kinds. Each category's contract is what it accepts.

- **Primitives**: `pill`, `tinted_icon`, `dot`, `button`, `muted_text`, `text_link`.
- **Rows and containers**: `item_row` (new: a leading child and a trailing child around `title`, `meta`,
  `meta_lines`, `chevron`; the two are `core://Children` `at: 0` and `at: 1` and stay so, so it needs no client
  build and no second file later), `grouped_list` (from the pod: rows in one card with hairlines between),
  `titled_section` (a label over its children), `card` (gains paddings), `callout` (title, body, tone), `stat`.
  Every row fits `grouped_list`; every section takes rows or cards.
- **Scaffolds**: `screen_scaffold` (a pinned header, a padded scroll, an optional pinned footer); `page_header`
  gains `weight`, a spinner while the screen loads and the load's error row; `two_pane`.
- **Patterns**: `counted_list` (one count query drives a grouped list and its empty note), `checkable_row`,
  `choice_chip` (writes an entity field or a local draft).

Names avoid what exists: `item_row` not `row` (plumbing `core://Row`), `titled_section` beside `section_header`
and `section_label`, `checkable_row` not `check_row` (a web tool). Every component that opens or toggles something
takes a `description` and sets `content_description`, which screen readers and [trace-from-home](trace-from-home.md)
both read.

**Core layouts on the core set** is a refactor: new layout files whose expansion draws the same pixels as the
files before them (the golden test of [plumbing-and-porcelain](plumbing-and-porcelain.md), run through the
simulator), with the same screen ids, edge keys and remembered-form scopes (CONTRIBUTING rule 5). Roots become
`core://Page`; no fixed colours. A test then fails when a core component is used by no core layout and by no
other component's example.

**Catching up** adds behaviour, one core layout a PR: events (going, directions, hiding past events), inbox
(new, earlier, older; a channel's icon and tint from its data), and new core layouts for server health and a task
queue. Settings needs nothing new: its sections are already parts (`@core/settings/slot:jacks`, …), so a pod that
wants its own rows writes a settings layout from those parts and its rows, and follows the parts.

The developer's pod is cleaned by its agent, not by a deploy (house rule "change what the user actually sees"):
aliases and dead components deleted, hand-built equivalents replaced, copies turned back into references where
the core layout has caught up.

### Template directives

- `{"type": "core://Each", "arg": "<json list argument>", "as": "t", "template": <node>}`: the template once per
  entry; `$t.<key>` reads the entry.
- `{"type": "core://Match", "arg" | "value", "cases": {"<key>": <node>}, "else"?: <node>}`. With `arg` the case is
  chosen at expansion. With `value` (a binding) it expands to one `core://Conditional` `equals` per case, and
  `else` to conditionals with `not` for each key, so plumbing is unchanged.
- Named slots: `{"type": "core://Children", "slot": "<name>"}` in a template, `"slots": {"<name>": [nodes]}` on a
  use. `children` stays the unnamed slot.

**References never ride inside a `json` argument.** `follows`, the cycle check, sealing, the trace from home and
the publish closure find references at three sites, and they stay the only three. A component that takes layouts
(`tab_shell`) takes them as `core://Layout` children in a named slot, each with its key; its `tabs` argument holds
only keys, titles and icons.

Compatibility: feature `component_templates`, recorded on an element whose definition uses a directive, as
`component_json_params` is. An older client is sealed the newest entry of that name it can read, and the latest
when none fits, which it draws as "This part isn't available.". So core components use the directives only once
clients with the feature are out, and a pod's agent is warned by the write when a definition needs a feature one
of the pod's devices lacks (`list_devices` knows their builds).

### Tones, theme scale

- `tone` on `pill`, `tinted_icon`, `callout`: `accent`, `success`, `error`, `muted` map to the built-in pairs
  (`accent` on `accent_surface`, `success` on `success_surface`, `error` on `error_surface`, `muted_text` on
  `muted_surface`); any other value is a token of the pod's own with its `<token>_surface`. Resolved at
  expansion; no client build. The pod's present `_soft` tokens are renamed by its agent.
- The theme gains numbers beside colours: `corner_s|m|l`, `pad_s|m|l`, `title_weight`. Feature `theme_scale`; the
  renderer must bind a number from `local://theme`. A component that reads them carries the feature, so an older
  client is sealed the entry before. Core components give each a default equal to today's value, so nothing moves
  until a pod sets one.

### Kept device values, dates

- `action.local.set` gains `"keep": true` (feature `local_keep`): the value is stored on the device with the
  remembered form values, per pod, at most 200 values of 1 KB, and forgotten when the device leaves the pod. It
  works offline; it does not sync between devices, which is the point. A pod moves its tab state out of
  collections only when its devices have the feature; the write hint for a literal row id says so.
- A `dates` feed computed by the pod in its time zone: `today`, `this_week`, `next_days:<n>`, one entity per day
  (`date`, `weekday`, `day`, `month`). No helper collections. The device already redraws at midnight for date
  conditions; the feed is refreshed on the same tick.

### Kits

A **kit** is one or more layouts and the definitions of the collections they read, the collections being its
inputs (a course has three). Using a kit creates the collections it lacks and checks the fields of those that
exist; names must be equal in the first version.

- **Core kits first.** They ship with the server like core layouts, written and reviewed in this repository, so
  they need no consent flow. Candidates from the audit: checklist, dated log, course, research and compare,
  swipe-to-rate deck, agenda, typed preference list. A fresh pod's home stays the blank page it is; the
  instructions point the agent at kits.
- **Shared kits later**, with the listed shelf. [sharing](sharing.md) strips a publication down to an interface
  on purpose. A definition can carry fillers, model prompts that spend the adopter's key, actions, alerts and
  URLs, so a shared kit's review lists each of those and adopting asks for each
  ([adopt-shows-urls](adopt-shows-urls.md) extended), and `summary`, `use_when` and `doc` text from another pod is
  capped in length, shown in the review, and marked as another user's words wherever a tool answers it. The shelf
  needs a `host.db` migration, moderation and limits; none of that is designed here.

## Migration, rollback, limits

- **Existing pods** follow `@core/` names at the deploy, as today. Layouts that copied a core layout don't;
  `copy_of` lists them and each PR that changes a core layout names the pods it won't reach. Adopted publications
  pin what they drew and are untouched.
- **Rollback.** Shipped files only go forward: a bad core component or layout is fixed by the next file. A pod's
  own names go back with `restore_version`, which is why history must stop filling with re-seals first.
- **Hints** add text to every write's answer; the cap is 10.
- **Tool surface**: 67 tools today. This adds `find_elements`, `guide` and `note_gap`; `list_parts` folds into
  `find_elements` when `blueprints` is removed.
- **Words**: this removes one noun (blueprint) and adds two an agent must know (scope, kit). Categories and tones
  are argument values, not concepts.
- **What still needs the server**: a new node the renderer draws, a new intent, a new source, a new jack. The
  goal is that screens over collections never do.

## Open

- Whether the 4,096-character cut is one client's or common. Under 2,000 is safe either way.
- How the root manifest serves a layout opened by name that home doesn't reach (today: looked up by name, off the
  tree). Its decision must answer it.
- Whether `doc` and `examples` should stop travelling to devices inside component elements (they do today), now
  that a sidecar holds what agents read.
- A kit over an existing collection whose field names differ needs a mapping; left out of the first version.

## Review

Two independent reviews of the first draft changed it as follows.

- Facts corrected against the source: 17 unused components, not 19; `core://IfArg` `equals` can already choose by
  key; `action.local.set` is written to the entity cache; `validate_layout`, previews and public links also
  expand; 67 tools, not about 80; the blueprint is keyed on by about ten code paths, so "only a word" was
  overstated; built-in theme pairs end in `_surface`.
- Design changed: question 3 now recommends the root manifest instead of relying on small elements, and fixes
  history first; references are kept out of `json` arguments; `category` and `use_when` moved to a sidecar because
  component files are append-only; the share of `core://Use` nodes was dropped as a measure; lettered stages became
  single features with their dependencies; settings uses its existing parts instead of a new slot mechanism; kits
  split into core (no consent needed) and shared (consent, flagged text); feature names and what older clients
  are sealed were added for each client-build change.
- Not verified by either review: the pod-side counts (a live pod only the audit could read), and how the engine
  draws a component definition it cannot read.
