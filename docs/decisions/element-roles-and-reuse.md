# Elements have a role and a scope; reuse is taught, hinted and made cheap

Status: **proposed, nothing here is built.** This file is the umbrella for the work below. Each stage lands as its
own feature PR with its own decision file (rule "same PR" in [README](README.md)); this file is then replaced by
links to them or deleted.

Date: 2026-11-20. Related: [content-addressed-construct](content-addressed-construct.md),
[one-layout-system](one-layout-system.md), [plumbing-and-porcelain](plumbing-and-porcelain.md),
[component-json-params](component-json-params.md), [blueprints-and-home](blueprints-and-home.md),
[layouts-not-apps](layouts-not-apps.md), [trace-from-home](trace-from-home.md), [sharing](sharing.md),
[custom-theme-tokens](custom-theme-tokens.md), [remembered-form-values](remembered-form-values.md),
[one-button-node](one-button-node.md), [mcp-connector](mcp-connector.md).

## What was measured

An audit of what the server ships and of the developer's own pod (2026-11-20). Shipped files and the pod's `home`
were counted by script; the pod's other 35 layouts and its 44 components were read in full and tallied by hand
(±15%).

- **Shipped**: 37 components (34 in `pod-core`, 3 in `source-pixelfed`) and 7 blueprints (542 nodes in their
  newest files). 69 of those nodes (13%) are `core://Use`. 19 of the 37 components are used by no blueprint and no
  other component: `button`, `chevron_card`, `date_block`, `divided_item`, `filled_card`, `footer_bar`,
  `grouped_card`, `labeled_text`, `muted_note`, `muted_text`, `screen`, `scroll_page`, `section_label`, `stat`,
  `text_link`, `title_meta`, `two_pane`, `workspace`, `pixelfed_sign_in` (the last is used from Rust, by the
  adapter's signed-out screen). The blueprints' roots are still `core://Workspace` (7 uses).
- **The pod**: 36 layouts and 44 `@me/` components. `home` is 672 authored nodes (97 KB), of which 52 (8%) are
  `core://Use`. About 15 of the 44 components are aliases of `@core/` ones, duplicates of each other, or used
  nowhere. Five layouts look copied from built-in ones (home, inbox, events, pixelfed, and a reworked settings); a
  copy doesn't follow what it was copied from.
- **Why nodes are written by hand**, in the order they cost most:
  1. A component's template can't repeat over a list argument or choose a branch by a key. The definition's only
     directives are `$arg`, `core://Children` (positional, `at: i`), `core://IfArg` and `core://Fragment`. So a tab
     bar, a pane switcher, a chip grid and a value→icon map are each unrolled (the dock's tab mutation is written
     30 times; 16 pane cases; 98 time-slot chips).
  2. Children are positional. `@core/icon_row` has no leading or trailing slot, so a row with a checkbox, a tag or
     a swatch is built by hand.
  3. No tones or states: a component with several states repeats its whole body per state.
  4. The theme carries colours only, so a restyle that changes corners or paddings needs parallel components and
     copied layouts.
  5. State that belongs to a device (the current tab) is kept in collection rows addressed by a literal row id
     (one row is bound 44 times in `home`). `core://ChoiceChips` `remember` is the only device-kept value today
     ([remembered-form-values](remembered-form-values.md)); `action.local.set` lasts while the app runs.
  6. Agents don't find what exists: `@core/muted_text` is used about 14 times in the pod while its hand-built
     equivalent appears about 150 times.

## Questions this answers

### 1. Is "blueprint" still a concept?

No, only a word. Since [content-addressed-construct](content-addressed-construct.md) a blueprint is an element
like any other, named `@core/<id>`, whose node is a `core://Page`. What makes it differ from `@core/card` is its
**role** (it is a layout: presented or embedded through an edge, with its own screens and routes), not that it is
built in. What its source declares beside it (`BlueprintInfo`: feeds, whether it needs a sign-in, notification
channels) is data about a source's layout, and would be the same data for any layout a source ships.

The word costs something. It names a third kind beside "layout" and "component" that isn't one, and it carries the
old model's habit: "start from a blueprint, copy it to change it". Copies are what the audit found stale.

Considered:

- **Keep "blueprint".** No churn, but agents keep learning a kind that doesn't exist.
- **"Public element".** Rejected: `public` already means two other things, a publication's visibility
  ([sharing](sharing.md)) and public web links ([public-web-links](public-web-links.md)).
- **No noun at all: describe an element by its role and its scope.** Taken.

Decision: **an element has a role and a scope, and nothing else is a kind.**

| | What it means | Spelled |
|---|---|---|
| Role **layout** | A screen: its node is a `core://Page`; reached through an edge (`core://Layout`, `action.navigation.open`) | "layout" |
| Role **component** | A template expanded in place by `core://Use`; no edge | "component" |
| Scope **core** | Ships with the server, moved by deploys | `@core/<name>`; "a core layout", "a core component" |
| Scope **own** | The pod's | a layout's id, `@me/<name>` |
| Scope **shared** | Published by another pod, adopted here | `@<handle>/<slug>` |

"Part" stays: `@core/<layout>/<part>` is a named piece of a core layout.

The rename, in the order it pays:

1. **What agents read** (MCP instructions, tool descriptions and tool answers): "blueprint" becomes "core layout".
   `list_layouts` answers `core_layouts` and keeps `blueprints` as an alias for one release of the connector;
   `list_components` rows say `kind: "layout"` or `"component"` with `scope`. `copy_of` stays (it names what a
   layout was copied from).
2. **Docs** (`docs/*.md`, `CLAUDE.md`, `CONTRIBUTING.md`): the same words. Old decisions keep their text; add a row
   to README's Renamed table.
3. **Code** (521 occurrences under `crates/`: `BlueprintInfo`, `Source::blueprints`, `HOME_BLUEPRINT`, …): renamed
   in one mechanical PR, then `scripts/check_old_names.sh` gains `blueprint` outside migration code.

No wire change, no client build. The Claude connector keeps its old tool list until "Refresh tools list".

### 2. Is "layout" still used, or is everything an "element"?

Both, for different things, and that stays. **Element** is the stored object: immutable, named by its hash, the
unit a device fetches and keeps. **Layout** and **component** are the two roles an element is used in. The role
decides real behaviour (a layout is an edge the navigation stack keys; a component is transparent), so prose,
tool names (`create_layout`, `define_component`) and the 4,271 uses of "layout" in code keep the role words.
"Element" is used where storage, hashes, sync or sharing are meant.

Rejected: calling everything an element. It would hide the one distinction authors must get right (edge or no
edge) and rename the whole tool surface for nothing.

### 3. Does the server send layouts inflated?

No. Since [one-layout-system](one-layout-system.md) a device asks `GET /v1/construct` for the root hash, then
`POST /v1/objects {have, want}` for the objects it lacks; the engine keeps them by hash (`construct_objects`) and
expands components itself (`construct-elements`, shared with the pod). A component used by twenty layouts is
fetched once. Only MCP's `expand` and `get_layout {expanded: true}` inflate, for the agent to read.

Two properties of this model decide how much it helps, and the audit shows the pod gets little of it:

- **The unit is the element.** Nodes written inline in a layout are part of that layout's one object. `home` is
  one 97 KB object; nothing inside it is shared or kept separately.
- **A change moves every element above it.** A reference holds its target's hash, so when a layout changes, every
  layout that reaches it is sealed again as a new object, up to `home`. `home` is at entry 682 of its history and
  the 20 newest are all re-seals. Each one is a new 97 KB object for every device to fetch, though nothing in
  `home`'s own nodes changed.

Decision: **keep the model; make elements small.** No wire change.

- `home` and other hubs hold structure only: embeds, links and component uses. Their own bytes are then a few KB,
  so a re-seal costs little.
- Repeated nodes become components (stages B and C below), so they are fetched once and survive edits elsewhere.
- The pod reports the cost so it can be seen: `list_layouts` adds each element's `bytes` and its `use_share` (the
  share of its nodes that are `core://Use`), and MCP writes warn when an element passes 32 KB.

Considered and left: sending a re-sealed element as a diff against the one the device has (saves bytes on the
wire, but adds a second object encoding and the device still stores both); making references by name only, with
no hash (ends re-sealing, but gives up "one hash fixes the construct", which sync and the stack rely on).

### 4. Does an agent get context when it connects, and can it be tuned for reuse?

Yes: the MCP `initialize` answer carries `instructions`, built by `pod_core::mcp::instructions`: the constant
`INSTRUCTIONS` (5,293 characters) followed by a paragraph from each source that has one (collections, events,
agents). It is the only text every agent reads before its first call.

Two findings:

- **It is longer than some clients show.** In the session that ran this audit the client delivered exactly the
  first 4,096 characters and cut the rest. Lost: how a `core://Page` draws its own header, theme tokens,
  `action.confirm`, jacks, sharing, and every source's paragraph (so also the collections guidance).
- **It says little about reuse.** One sentence ("Prefer a component over repeating nodes…"), placed after 3,000
  characters about navigation. It never says to look at what exists before writing nodes, and nothing at the
  point of writing says so either.

Decision: **short instructions that put reuse first, detail on demand, and hints where the writing happens.**

- **Instructions under 2,000 characters**, a test fails above that. In this order: what the pod is; the build
  procedure (below); "change what is drawn, not what it was copied from"; where the detail is. Everything else
  moves to a new read-only tool `guide {topic}` (topics: `navigation`, `pages`, `theme`, `actions`, `jacks`,
  `sharing`, and one per source); a source's `mcp_instructions` becomes its topic, and the instructions list the
  topic names. Tool descriptions keep the rule that applies to that tool.
- **The build procedure**, as the instructions state it:
  1. Find what is there: `list_layouts`, then `find_elements {query}` (below).
  2. Use a core or shared layout whole when one fits; bind it to the user's data.
  3. Otherwise compose: a scaffold, sections, rows, all components. Write plumbing only for what no component
     draws.
  4. A piece that appears twice is a component: `define_component` before the second copy.
  5. Look at it (`render_layout_preview`) and read the write's hints.
- **`find_elements {query?, role?, category?}`** replaces reading the whole of `list_components` (52 KB today, one
  flat list). Each component and core layout gets `category` (`primitive`, `row`, `container`, `scaffold`,
  `pattern`) and a `use_when` line in its definition file; the tool answers name, role, scope, category, `use_when`
  and parameters, ranked by the query. `list_components` stays.
- **Reuse hints on every layout and component write**, added to the `warnings` a write already answers
  (`validate::fixed_colours`, `validate::button_faces`):
  - a subtree whose shape is what a core or own component expands to, with the component and arguments to use;
  - a subtree repeated three times or more in the write (shape equal with literals and bindings set aside);
  - a literal row id in a binding (`collections://<c>/<row>#<field>`);
  - the element's size when above 32 KB.
  Hints are advice: the write succeeds.
- **A number to steer by**: `use_share` per layout (question 3) and for the pod. The stages below are judged by
  whether it rises on a pod built from nothing by an agent. A test in `crates/e2e` builds three reference screens
  through MCP from scripted tool calls and asserts the hints fire; it can't judge a model's choices, so a manual
  run (a fresh temporary pod on staging, one prompt per reference screen) goes in each stage's PR.

Considered: keeping one long instruction (already cut by a client); putting the guidance in tool descriptions only
(an agent that never lists components never reads it); refusing writes that repeat nodes (blocks legitimate
one-offs and makes agents fight the validator).

## Stages

Each is one feature and one PR. A to C need no client build.

| | Stage | Client build |
|---|---|---|
| A | Words: questions 1 and 2 | No |
| B | Teaching: question 4, plus `bytes` and `use_share` (question 3) | No |
| C | Core set: the layered components, core layouts catching up | No |
| D | Templates: repeat, switch, named slots | Yes (the engine expands components) |
| E | Theme scale and tones | Yes (renderer) |
| F | Device-kept values and a dates feed | Yes for device-kept values; the feed is pod only |
| G | Kits and a listed shelf | No |

### C. The core set

Four layers. Each layer's contract is what it accepts, so pieces fit without reading their templates.

1. **Primitives**: `pill`, `tinted_icon`, `dot`, `button`, `muted_text`, `text_link`.
2. **Rows and containers**: `row` (new: leading and trailing children, `title`, `meta`, `meta_lines`, `chevron`;
   until stage D the two slots are `core://Children` `at: 0` and `at: 1`), `grouped_list` (from the pod: rows in one
   card with hairlines between them), `section` (a label over its children), `card` (gains paddings), `callout`
   (new: title, body, tone), `stat`. Every row fits `grouped_list`; every section takes rows or cards.
3. **Scaffolds**: `screen_scaffold` (a pinned header, a padded scroll, an optional pinned footer; `page_header`
   gains `weight`, a loading spinner and the load's error row), `two_pane`, and after stage D `tab_shell`.
4. **Patterns**: `counted_list` (one count query drives a grouped list and its empty note), `check_row`, `choice_chip`
   (bound to an entity field or a local draft).

With it:

- Core layouts are rewritten on the set (new layout files; roots become `core://Page`; no fixed colours), so they
  are the worked examples `get_layout` shows. A test fails when a core component is used by no core layout and no
  example of another component.
- Core layouts take what the pod's copies added, so the copies can go back to references: events (going,
  directions, hiding past events), inbox (grouping by new, earlier and older; a channel's icon and tint from its
  data), settings (a slot for a pod's own rows), and new core layouts for server health and a task queue.
- The developer's pod is cleaned by its agent, not by a deploy: aliases and dead components deleted, hand-built
  equivalents replaced, per the house rule "change what the user actually sees".

Not in the set: anything that names a collection or a field. Those are kits (stage G).

### D. Templates that repeat and choose

- `{"type": "core://Each", "arg": "<json list argument>", "as": "t", "template": <node>}`: the template once per
  entry, `$t.<key>` reading the entry.
- `{"type": "core://Match", "arg" | "value", "cases": {"<key>": <node>}, "else"?: <node>}`: with `arg`, chosen when
  expanded; with `value` (a binding), it expands to one `core://Conditional` per case, so plumbing is unchanged.
- Named slots: `{"type": "core://Children", "slot": "<name>"}` in a template, `"slots": {"<name>": [nodes]}` on a
  use; `children` stays the unnamed slot.

All three are expansion only: what the renderer draws is today's plumbing. The engine expands components, so a
build without them can't read a definition that uses them. As with `component_json_params`: a new feature
(`component_templates`), recorded on elements whose definition uses it, and an older client is sealed the newest
entry of the name it can read. Core components don't use the directives until those clients are out.

This is what lets `tab_shell {tabs: [{key, title, icon, ref}]}` exist: one list drives the phone's dock and the
wide window's sidebar and detail pane.

### E. Theme scale and tones

- The theme gains a scale beside its colours: `corner_s|m|l`, `pad_s|m|l`, `title_weight`. Core components read
  them, so a pod restyles by `set_theme` instead of by copying.
- `tone` on `pill`, `tinted_icon`, `callout` and `button`: `accent`, `muted`, `success`, `error`, or a token of the
  pod's own with its `_soft` pair. A tone is a pair of theme tokens, resolved at expansion.

Numbers in the theme need the renderer to bind a number from `local://theme`; that is the client build. Tones need
none.

### F. Device-kept values and a dates feed

- `action.local.set` gains `"keep": true`: the value is kept on the device across starts, in the engine's store
  beside remembered form values. The current tab and a filter stop being collection rows.
- A `dates` feed the pod computes in its time zone (`today`, `this_week`, `next_days:<n>`: one entity per day with
  `date`, `weekday`, `day`, `month`), so a pod needs no helper collections to list days.

### G. Kits and a listed shelf

A **kit** is a publication whose closure holds a layout (or several) and the definition of the collection it reads,
with the collection as its one input. Adopting a kit over a new collection creates the collection from the
definition; over an existing one it checks the fields. Core kits ship with the server under `@core/` the same
way. Candidates the audit found in one pod: checklist, dated log, course (lessons made of blocks, with
completion), research and compare, swipe-to-rate deck, agenda, typed preference list.

The listed shelf is [sharing](sharing.md)'s `public` visibility, refused today. It needs moderation and limits, as
that decision says, and is the last thing here.

## Consequences

- Stage A changes words agents see; the connector needs "Refresh tools list" once. Stages D, E and F each need new
  desktop and Android builds, and each says so in its PR.
- Layout hints make writes answer more text. They are capped at 10 per write, most valuable first.
- Rewriting core layouts on the core set is a new file per layout; pods that reference them follow, pods that
  copied them don't (`copy_of` lists them, and the PR names them).
- `find_elements` and `guide` are two more tools on a surface of about 80. `list_parts` folds into `find_elements`
  when the connector alias period ends.

## Open

- Whether the 4,096-character cut is one client's or common. Stage B doesn't depend on the answer: under 2,000 is
  safe either way.
- The shape matcher for "this subtree is component X": exact shape after normalising literals is cheap and
  misses near-matches (a different padding). Start exact; measure how often it fires on the developer's pod.
- Whether `core://Each` should also accept a query (a list of entities). No: that is `core://List`; `Each` is over
  an argument, at expansion.
- Kits over an existing collection need a field mapping when names differ; the first version requires equal names.
