# A kit used again brings in what it gained: its new optional fields and its own fillers

Date: 2026-11-28. Related: [shopping-kit](shopping-kit.md), [element-roles-and-reuse](element-roles-and-reuse.md).

The shopping kit's categories gained words to avoid and prefer when exploring, carried by each round to the taste
service. A pod that used the kit before has its collections already, and `use_kit` only checked an existing
collection for the kit's fields: it refused one that lacked the new ones, and never changed the kit's own fillers.
Considered: a migration per kit version (code for each change, run on every pod) and asking the owner to add the
fields by hand (they don't know the shape). Taken: using the kit again upgrades it.

Decision:

- **`use_kit` on a kit of several collections** adds, to a collection of the kit's that exists, each field the
  kit has and it lacks, when the field is optional or has a default (a required one is still refused,
  `collection_mismatch`), then brings each of the kit's fillers (matched by title) to the kit's spec, keeping its
  id, when it differs. Rows, other fields, other fillers and the owner's changes to fields are left alone.
- **A taste filler's changed spec explores nothing again** (rounds are explored once), so an upgrade changes only
  what later rounds ask.
- **The shopping kit's categories** gain `explore_avoid` and `explore_prefer`; needs look them up as `avoid` and
  `prefer`; rounds and pieces carry them (a round's are written when it is made, from its need or, for Go this
  way, from the piece); the explorer sends them to the service, which leaves out and ranks up by them.

Consequences: the layouts that make rounds have new versions (`shopping_need/0002`, `shopping_explore/0002`); no
client build. A pod gets the new fields when the kit is used again (Claude does it with `use_kit`).
