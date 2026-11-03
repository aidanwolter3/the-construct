# Claude adds one requested row without a preview (`add_row`)

Date: 2026-11-03. Related: [collections](collections.md), [mcp-connector](mcp-connector.md).

Decision `collections` makes every MCP row write go through `edit_rows`, which first answers a preview the user
must agree to. That fits imports and backfills, where Claude decides what the rows are. It is wrong for the
commonest write: the user tells Claude about one thing ("log that I had the oil changed", "add milk"). The
user has already said exactly what to write, so the preview only adds a round trip and a question.

Considered: a `skip_preview` flag on `edit_rows` (too easy to set on a bulk edit); a per-collection setting
(the same collection takes both kinds of write).

Decision: a separate MCP tool, `add_row {collection, values, note?}`, creates exactly one row at once. It shares
`edit_rows`' write path (validation, defaults, computed fields, usage caps, change events) and is recorded as an
edit, so `undo_edit` reverts it. Several rows, and any update, delete or restore, still go through `edit_rows`
and its preview.

The first use is the Log custom app (a `log` collection of things that happened, by category), which Claude fills
from conversation.
