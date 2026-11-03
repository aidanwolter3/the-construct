# `core://List` `focus`: scroll to one item and tint it (schema 12)

Date: 2026-11-03. Related: D34, D36, D43, [comment-threads-from-notifications](comment-threads-from-notifications.md).

Opening a comment notification should show the whole thread with that comment highlighted and in view. Layouts had
no way to scroll to anything, and highlighting one item needed either a per-request field on shared entities (the
entity cache would keep the tint on the next visit) or a duplicated item template behind a `Conditional` (there is
no "not equals").

Decision: `core://List` takes an optional `focus: {uri, background?, content_color?}`. The item whose entity URI
equals `uri` (route parameters are bound first, so `pixelfed://comments/$route.comment` works) is scrolled into view
once, the first time the list draws it (`BringIntoViewRequester` with about 96 dp of room above and below; not again
after Back or rotation), and drawn on `background` with `content_color` (`#RRGGBB` literals, checked by
`validate.rs`). No matching item: nothing happens. The highlight lives in the layout, so the shared entities stay
untouched.

Schema 12, additive: `protocol::SCHEMA_VERSION` 11 -> 12, no `RETIRED` entry. Schema 11 clients keep getting the
newest layout they can draw. Limits: one-column lists only (a grid ignores it); one focused item per list.
