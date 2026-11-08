# Layout edits show live by swapping the whole layout in place, not by Merkle-hashed subtrees

Date: 2026-11-08. Related: D32, [rust-clients-on-the-engine](rust-clients-on-the-engine.md),
[layouts-not-apps](layouts-not-apps.md).

A layout edited over MCP or from another device should show on an open device without leaving and reopening it,
keeping scroll, form values and the back stack. The idea on file was a BLAKE3 Merkle tree over the expanded layout:
the pod sends root digests over SSE and the client fetches and patches only the changed subtrees, so the Compose
UI's slot table keeps the unchanged ones.

That was a Compose problem. The Rust renderer lays out and draws the whole tree every frame and keeps no state in
the tree: scroll offsets are keyed by semantic id, form values by field, and the back stack holds layout ids,
routes and items. Swapping a layout's root keeps all of it. A layout is tens of kilobytes, fetched only when its
version changes, so diffing subtrees would save little and add hashing on the pod, a patch format and a second
way to apply a layout.

Decision: the pod keeps announcing `(layout, version)` on home's event stream; the engine fetches the whole layout
when it holds it and the version is newer, and the session swaps the new root in wherever it shows. Two gaps are
closed with this decision:

- **Open screens** (`target_uri` screens of a `core://Page`) draw the new version of their screen (same `id`),
  with their route, item and scroll kept; a screen the new version dropped stays as it was until closed.
- **Layouts never opened on the device** are noted too (the engine's `announced_json`), so the device's list of
  layouts (`local:layouts`, `local:all_layouts`, and home's own `local://layouts/home`) loads again and its titles
  and versions follow every edit, new layout and delete.

Not done: hiding or showing a layout from MCP (`set_hidden`) bumps no version, so other devices' lists follow it at
their next reload rather than at once.
