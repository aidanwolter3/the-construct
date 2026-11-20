# Elements have a role and a scope; reuse is taught, hinted and made cheap

Date: 2026-11-20. Related: [content-addressed-construct](content-addressed-construct.md),
[one-layout-system](one-layout-system.md), [plumbing-and-porcelain](plumbing-and-porcelain.md),
[component-json-params](component-json-params.md), [layout-features](layout-features.md),
[blueprints-and-home](blueprints-and-home.md), [layouts-not-apps](layouts-not-apps.md),
[trace-from-home](trace-from-home.md), [sharing](sharing.md), [custom-theme-tokens](custom-theme-tokens.md),
[remembered-form-values](remembered-form-values.md), [one-button-node](one-button-node.md),
[mcp-connector](mcp-connector.md), [previews-read-like-a-device](previews-read-like-a-device.md).

An audit of what the server ships and of the developer's own pod found that agents write screens out node by
node. The pod's `home` was 672 authored nodes (97 KB) of which 8% were component uses; the shipped layouts were
13%. Seventeen of 37 shipped components were used by nothing. About 15 of the pod's 44 own components were
aliases of shipped ones, duplicates of each other, or dead, and five of its layouts were copies of shipped ones
that had fallen behind them. The goal this serves: a new user talks to their agent and gets their interface
from what the server ships, without anyone upstreaming an element first.

Why nodes were written by hand, most costly first: a template could not repeat over a list or choose by a key;
children were positional, so a row with something at either end was built by hand; a restyle of corners or
paddings needed parallel components; a device's own state (the open tab) lived in collection rows addressed by a
literal row id; and nothing told an agent what already existed, neither before it wrote nor while it wrote.

Decision: **an element has a role and a scope; the pod teaches reuse where agents read and where they write;
templates repeat, choose and take named slots; a device keeps its own values; and a construct's root is a
manifest, so an edit costs one element.**

## Words

"Blueprint" named a kind that isn't one. A blueprint is stored like every element, named `@core/<id>`, with a
`core://Page` as its node; what sets it apart is that it is used as a layout and that a source declares data
beside it. The word also carried the habit "copy it to change it", and copies are what the audit found stale.

| | Meaning | Spelled |
|---|---|---|
| Role **layout** | A screen: reached through an edge (`core://Layout`, `action.navigation.open`) | "layout" |
| Role **component** | A template expanded in place by `core://Use`; no edge | "component" |
| Scope **core** | Ships with the server (the core's or a source's), moved by deploys | `@core/<name>`: "a core layout", "a core component" |
| Scope **own** | The pod's | a layout's id, `@me/<name>` |
| Scope **adopted** | Another pod's publication, adopted here | `@<handle>/<slug>` |

"Element" stays the word for the stored object (immutable, named by its hash, what a device fetches); "layout"
and "component" are what it is used as, and the difference is real (an edge or none), so tools and prose keep
both. "Public element" was rejected: `public` already means a publication's visibility and public web links.

The word left the code (`CoreLayoutInfo`, `Source::core_layouts`, `HOME_CORE_LAYOUT`), the tool text and answers
(`list_layouts` answers `core_layouts`, and `blueprints` too until the next change to that answer;
`list_components` rows say `role` and `scope`) and the guides. Migration code and older decisions keep it;
`scripts/check_old_names.sh` holds the rest. No wire change.

## What an agent reads

The MCP `instructions` were 5,293 characters, then a paragraph per source: about 18,600 in all. A client showed
its agent the first 4,096. Reuse had two sentences, the second 3,500 characters in.

- **Instructions under 2,000 characters** (`mcp::MAX_INSTRUCTIONS`, held by a test), in this order: what the pod
  is; a build procedure that starts with looking at what exists; "change what is drawn, not what it was copied
  from"; the names of the guide's topics.
- **`guide {topic}`**: the detail, by topic (`navigation`, `pages`, `components`, `theme`, `actions`, `jacks`,
  `sharing`, one per source, and `all`). A source's `mcp_instructions` is its topic; `mcp_summary` is its few
  words in the list.
- **`find_elements {query?, role?, scope?, category?}`**: what exists to build with, by what it is for. Rows are
  small: name, role, scope, category, a `use_when` line, parameters. `category` and `use_when` for what ships are
  in `catalog.json` beside the files (`crates/pod-core/components/catalog.json`, `crates/source-*/catalog.json`,
  `Source::element_catalog`), not in the component files, which are append-only. A test fails when something
  shipped has no entry.
- **Hints on every layout and component write**, in the `warnings` a write already answered
  (`pod_core::hints`): a subtree that is what a component expands to, with the use to write (exact: the
  component's parameters are wildcards, every other value must be equal); a piece written three or more times,
  equal but for its values; an entity with a generated-looking id written into a binding; and the write's cost
  when a device must fetch more than 64 KB again. Advice only, at most 10, worked out after the write within a
  time budget.
- **`note_gap {what}`**: when nothing fitted, the agent says what shape it lacked. The pod keeps the last 100
  lines; the operator reads them with `server pods gaps`.

Rejected: one long instruction (already cut by a client); guidance in tool descriptions only (an agent that never
lists components never reads it); refusing writes that repeat nodes (blocks honest one-offs).

## The core set

Components taken from what the audited pod wrote by hand, each with a catalog entry and a category
(`primitive`, `row`, `container`, `scaffold`, `pattern`): `grouped_list`, `item_row` (a leading and a trailing
child), `titled_section`, `callout`, `counted_list` (a list in a card or its empty note, from one count),
`checkable_row`, `choice_chip`, `screen_scaffold` (header, scrolling content, pinned footer, by named slots) and
`tab_shell` (one list of tabs as a phone's dock and a wide window's rail). `pill` and `tinted_icon` take a
`tone`; `card` its paddings; `page_header` a weight, a least height and a spinner.

Core layouts built on it: Events 0008 (with what the pod's copy had added: going, New and Going tags, past
events hidden; the `events` collection gains `going`), the inbox 0007 (New, Earlier, Older in grouped cards) and
a new one, `server_health`, for the `monitor` source, which shipped none.

Not done: the Agents, Pixelfed and Settings layouts are as they were. A tone is one of the theme's four pairs;
a colour of the pod's own is still two colours. `item_row` has no line above its title (the inbox's row needs
three lines and is plumbing).

## Templates that repeat, choose and take slots

Feature `component_templates`, recorded on an element that uses one; a client without it is sealed the newest
entry of the name it can read.

- `core://Each {arg, as, template}` repeats over a `json` list argument (at most 200 entries). Inside,
  `$arg.<as>` is the entry, `$arg.<as>_<key>` a key of it and `$arg.<as>_index` its place.
- `core://Match {arg | value, cases, else?}`: by an argument, one case is kept at expansion; by a binding, every
  case stays under a `core://Conditional`.
- `core://Children {slot}` takes the nodes a use passes as `slots: {name: [nodes]}`; `core://IfArg {slot}` tests
  whether a slot was filled. `children` stays the unnamed slot.

References never ride inside a `json` argument: a component that takes layouts takes them as `core://Layout`
nodes in a slot, so the three sites the pod follows stay the only three.

## What belongs to a device, and dates

- `action.local.set` with `keep: true` on a `local://` entity (feature `local_keep`): the device keeps the
  fields across starts and puts them back before its first frame. Per pod, at most 200 values, never sent to the
  pod. `tab_shell` keeps its open tab this way.
- The `dates` source (`crates/source-dates`): one feed, `days`, with the usual windows as queries, in the pod's
  time zone. Devices are told to load it again when the pod's day turns.

## Kits

A kit is a core layout that reads its collection through inputs (`$in.collection`, `$in.title`), so one layout
serves any collection of its shape, by reference. MCP `use_kit` makes the collection (or checks that one of that
name has the kit's fields), has home bind its feed, and answers the embed and the link with the inputs bound. A
layout is served by an edge's own key too (`checklist-packing`), so previews draw a bound kit.

One kit ships: `checklist`. The audit found others in one pod (a dated log, a course, research and compare, a
swipe-to-rate deck, an agenda, a typed preference list); none of those is built. Kits other pods publish, and a
listed shelf to find them on, are not built: a shared kit's collection definition can carry fillers, model
prompts and URLs, which needs a consent flow ([sharing](sharing.md) strips them on purpose).

## The root is a manifest

A reference held its target's hash, so when an element changed, every element above it was sealed again as a new
object, up to `home`: each one fetched whole by every device, and an entry in its name's history, which keeps
500. The audited `home` was at entry 682 and its 20 newest were all re-seals.

Considered: keeping elements small (fewer bytes per re-seal, the same number of objects, and more components
widen the fan-out); references by name only (gives up one hash fixing the construct); sending a re-sealed
element as a diff (saves the wire only).

- **A client that asks** (`x-construct-root: manifest`) is told, by `GET /v1/construct`, the `construct` event
  and `GET /v1/construct/names/{name}`, the hash of `{"kind": "manifest", "home", "names": {name: hash},
  "scale"?}`: every name home reaches, each at the entry of its history that client is served. The elements it
  is sent hold references that follow a name without its hash, and no examples
  (`construct_elements::stripped`), so an element's hash moves only when it is edited. One hash still fixes the
  construct. A header, not a layout feature: it changes how a construct is fetched, not what is drawn.
- **The pod's store is unchanged**: it still seals what it stores, and a client that doesn't ask is sent home
  sealed as before. The manifest is how the same construct is served.
- **The engine** asks for it, resolves references through it (`Objects::resolve`), keeps what it names, and
  draws a layout again when what it expands to changed though its own element did not.
- **History**: re-seals in a row are one entry and don't count toward the 500.
- **The cost is reported**: `list_layouts` gives each layout's `bytes`; a write answers `refetch: {objects,
  bytes, sealed: {objects, bytes}}`.

In the tests an edit of a component two layouts use sends a manifest client two objects (the component and the
manifest) where a sealed client is sent four.

Not on manifests: the web client and the simulator's live mode, which are sent home sealed.

## The theme scale

`set_theme {scale}` sets `corner_s`, `corner_m`, `corner_l`, `pad_s`, `pad_m`, `pad_l` (dp) and `title_weight`.
A component's template reads them as `$theme.<name>` when it is expanded (feature `theme_scale`); the pod's own
expansions use the pod's values, and the manifest carries them to devices, so a new scale is a new root. Tags
read `corner_s`, cards and callouts `corner_m`, grouped lists `corner_l`, page titles `title_weight`; the
defaults are what they drew before. No core component reads the paddings yet. A client sent home sealed draws
the defaults.

## Stack depth

Components nest boxes in boxes: Events 0008 expands 21 deep where 0007 was 14. Building a node is one large
frame per level, and that page needed up to 1 MB of stack in a release build and up to 4 MB in an unoptimized
one. The renderer now grows the stack where it recurses (`stacker`, in `layout.rs`), previews draw on a thread
of their own, and the server's runtime threads have 16 MiB. With that, each of the new pages draws in 256 KB.

## Consequences

- **New desktop and Android builds are needed** for: template directives and named slots, kept device values,
  the theme scale, and fetching by manifest. An installed build keeps working: it is sent home sealed and the
  newest entry of each name it can read, so it draws Events 0007 and the inbox 0006.
- Pods that reference the core Events or inbox layout draw the new ones after the deploy. Pods that copied them
  don't; `copy_of` lists them. The developer's pod uses its own copies of both.
- Connected agents see the new tools and wording after "Refresh tools list".
- Not checked on a device: the Android app and the Mac window (kept values across a real restart, the manifest
  over a real connection, the stack on a phone's threads).
- `core://ChoiceChips` `remember` is stored by the engine and read by no Rust client; found here, not changed.
