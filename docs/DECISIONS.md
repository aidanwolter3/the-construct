# Design Decisions

Short records of places where the implementation resolves ambiguity or deviates from DESIGN.md.

## D1: `MediaCarousel.media_refs` accepts a binding expression or a literal list

DESIGN.md §2.2A types `media_refs` as `Vec<String>`, but the hydration bundle in §2.2C sends
`"media_refs": "$item.media_uris"`, a template binding string. Item templates need bindings,
and hand-written layouts want literal lists, so both forms are accepted.

Decision: `media_refs` has type `protocol::ListBinding`, an untagged enum:

- `ListBinding::Expr(String)` for a JSON string such as `"$item.media_uris"`
- `ListBinding::List(Vec<String>)` for a JSON array of entity URIs

It is untagged, so each form serializes back exactly as it arrived. The renderer resolves
`Expr` against the current item or entity at render time.

## D2: `WorkspaceSpec` is the hydration bundle

DESIGN.md names `WorkspaceSpec` without defining it. In this codebase `protocol::WorkspaceSpec`
is the §2.2C bundle: `{ version, workspace: UiNode, entities: { uri -> EntityRecord } }`.
`EntityRecord` is a schemaless JSON object (`serde(transparent)` over a JSON map), so any
entity fields round-trip unchanged. Typed domain views can come later.

## D4: Counter demo uses `OptimisticMutate`, plus a minimal expression subset

The M2/M3 counter layout (`fixtures/counter_home_bundle.json`, served at `GET /v1/workspaces/home`)
reuses `ActionIntent::OptimisticMutate` on the entity `core://counters/home`, field `count`. It adds
no new intent variant. Increment uses `value_expr: "entity.count + 1"` and Reset uses `"0"`. The
endpoints `/v1/counters/home/{increment,reset}` are informational for now: there is no outbox
until M5.

Until a real CEL evaluator exists, a small expression subset stands in:

- **Mutation `value_expr`**, evaluated in client-core against the target entity's current
  payload: a JSON literal (`0`, `true`, `"x"`); `entity.<field>`; `!entity.<field>` (boolean
  negation); `entity.<field> + N` and `entity.<field> - N` (integer arithmetic). Anything else
  logs a warning and changes nothing.
- **Text `value_expr` interpolation**, done by the renderer: each `${<entity_uri>#<field>}` is
  replaced by that entity field's value, or by an empty string when it is missing. Inside an
  item template, an expression that is exactly `entity.<field>` or `$item.<field>` resolves
  against the current item. Any other text renders as-is.

## D3: Absent optional fields are omitted

`AuthorHeader.on_click` is `Option<ActionIntent>`. It is left out of the JSON when `None` and
defaults to `None` when missing, which matches the §2.2C fixture.
