# The pod says how home reaches each layout and which layouts are copies of blueprints

Date: 2026-11-17. Related: [content-addressed-construct](content-addressed-construct.md),
[layouts-not-apps](layouts-not-apps.md), [notifications-in-a-collection](notifications-in-a-collection.md).

The inbox change of decision `notifications-in-a-collection` shipped in the `inbox` blueprint, but the developer's
pod didn't show it: their home's swipe down from the top opens `c-ejdjtgh6dx`, their own copy of the old inbox,
which doesn't follow `@core/inbox`. Nothing told the agent that: `list_layouts` listed layouts and blueprints side
by side, with no link between what home draws and the names. Finding it took reading home's tree by hand. Most
agents that change a user's screens work through the pod's MCP server alone (Claude on a phone, with no
repository), so a rule only in `CLAUDE.md` would miss them.

Decision: **the pod answers where things are drawn, and its MCP instructions say to use it.**

- `pod_core::construct::trace::from_home` follows the construct's DAG from home: each element materialized (its
  components expanded), each edge (`core://Layout`, `action.navigation.open`, in a gesture or a tap) followed
  breadth first, so every name reached gets its shortest path, and each step says how it is taken: the field that
  holds it (`on_click`, `on_swipe_down`, `embedded`), the presentation and edge, and the tap target's label
  (`content_description` or `label`).
- `list_layouts` and `get_layout` give each layout `reached` (`home → (on_swipe_down, Overlay from top) → c-…`, or
  "not reached from home"), and `copy_of`: the blueprint a layout is titled like but doesn't reference (a
  heuristic: the pod keeps no record of where a converted layout came from). Blueprint rows add their own
  `reached`, `used_by` (the layouts that reference them) and `copies`.
- The pod's MCP instructions: before changing what the user sees, start at home, follow its references to the
  element drawn there (`reached`, `copy_of` help), change that element rather than the blueprint it may have
  started from, and look at it with `render_layout_preview`. `CLAUDE.md` says the same for repository sessions
  changing blueprints: check what real pods draw, and say which copies a change won't reach.

Not done: a copy renamed away from its blueprint's title isn't flagged; paths through a layout's own screens
(`$route`) or through notifications aren't followed. Connected Claude clients see the new fields and
instructions once they refresh the connector's tool list.
