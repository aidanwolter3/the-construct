# A component parameter may carry any JSON value: an intent, a count query

Date: 2026-11-19. Related: [plumbing-and-porcelain](plumbing-and-porcelain.md) (parameters were values only),
[content-addressed-construct](content-addressed-construct.md), [named-vocabulary](named-vocabulary.md).

A component's parameters were values only: `text`, `number`, `bool`, `uri`, `binding`. So a component could draw
a face but not what it does or what it shows for: an `on_click`, a `core://Conditional`'s `count`, a list's `where`
had to stay at every use site. The pod's own layouts show the cost. The empty state "a muted line while this list
is empty" (`core://Conditional {count, not: true}` around a muted text) is written out in nine of one pod's layouts.
`core:muted_note` could take only the text, and every caller still wraps it. A quiet text link (an accent Label
text in a `core://Tappable`) repeats in three, and no component can hold it, because its `on_click` differs.
The template already substitutes a whole `"$arg.n"` with the argument's own JSON type. Only the parameter types
stopped an object from getting there.

Decision: a parameter may have type `json`, any JSON value except null. A whole `"$arg.n"` in the template puts
it in as it is. It is meant for an intent (`on_click`), a `count` query (`{query_key, where?}`), a list of `where`
conditions, or a `bind`. Nothing else changes. The expansion is plumbing, and it is validated as plumbing, as before:
an argument that isn't a valid intent where an intent goes is refused on the write, with the pointer.

- **Engine.** The engine expands components too (`client_core::construct` parses each element's definition), so
  a build without the type can't read a definition that uses it. That build would draw the use as "This part isn't
  available.". New feature `component_json_params`: the pod records it on an element whose definition has a `json`
  parameter, the way it records `layout_bind`. Sealing then serves an older client the newest entry of that name
  it can read. **This needs a client build** before a pod's components can use the type on those clients. Nothing
  existing changes: no definition uses the type yet.
- **Still values first.** Prefer `text`, `number`, `bool`, `uri`, `binding` where they fit. They say what the
  argument is, and a wrong one is refused at the use rather than in the expansion. `json` is for the parts of a
  node that are objects or lists.

Since (issue #405, once clients with the feature were out): `core:muted_note` 0002 takes an optional `count`, so a
use draws the whole empty state, and `core:text_link` takes its `on_click`. A client without the feature is served
`core:muted_note` 0001, which ignores `count` and always shows the note, and draws `core:text_link` as "This part
isn't available.".
