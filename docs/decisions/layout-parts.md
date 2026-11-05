# Custom layouts reuse built-in parts by reference; copies get an update notice

Superseded by [plumbing-and-porcelain](plumbing-and-porcelain.md) for references: parts are components, and `core://Ref` is an alias of `core://Use`.

Date: 2026-11-03. Related: D36, [custom-apps-and-data-sources](custom-apps-and-data-sources.md),
[mcp-connector](mcp-connector.md), issue #45 (P3).

A custom app often starts from a built-in one ("a compact Pixelfed", "Kitchen with the normal post screen"). If it
copies the built-in's layout, the copy falls behind every time the built-in ships a feature. There was no reuse
mechanism: no fragments or includes; even `pixelfed/0005.json` repeats its post card.

Decision:

- **Parts:** every version of a built-in app has parts: each screen (`screen:<id>`), each slot's content
  (`slot:<id>`), and the named parts its layout file declares next to `layout` (`"parts": {"post_card": <node>}`;
  names `[a-z0-9_]+`, the same checks as the layout, hashed with the file). Declared parts are stored in the
  version's `meta_json`.
- **References:** a custom layout writes `{"type": "core://Ref", "app": <built-in>, "part": <name>, "version"?: n}`
  (a screen part in `screens`, any other part as a node). It is pod-only: the pod expands it when serving the
  workspace and the version history (`pod_core::parts`), from the built-in's newest version the client can draw, or
  the pinned `version`. Clients never see a ref, so no schema bump. A write is validated expanded, so what a part
  brings (routes, query keys, entities) must fit the custom app's sources; it is stored as written. A ref that can't
  be expanded later is served as "This part isn't available." (a screen ref is left out).
- **`ref_update` versions:** when a deploy appends versions of built-in apps, the pod appends one version (source
  `ref_update`, author `pod`, note "Pixelfed updated to v6") to each custom app whose latest layout references one
  of them without a pinned version. History stays monotonic and says why the app changed, and clients refetch.
- **Copies:** a write may say `copied_from: {app, version}` (checked: a built-in's existing version); later versions
  inherit it, a restore takes the restored version's. `GET /v1/apps` adds `updates_from: {app, title, from_version,
  to_version}` to such an app while that built-in has a newer version the client can draw that the user hasn't
  dismissed (`POST /v1/custom-apps/{id}/dismiss-update {version}`, any session; pod setting `updates.dismissed`).
  The menu shows "<built-in> has updates · ask Claude to bring them in" with Dismiss (engine/FFI
  `dismiss_update`).
- **Claude:** MCP `list_parts` (parts of built-in apps, optionally with their layouts), `rebase_context` (the copied
  base, the built-in's latest and the custom layout, to merge), `dismiss_update`, and `copied_from` on `create_app` /
  `update_layout`. The instructions say: reference by default, copy only what changes. Accepting a rebase is an
  `update_layout` with the new `copied_from`; rejecting it is `dismiss_update`.

Not done yet: no built-in layout file declares named parts yet (screens and slots are parts already); a new
`pixelfed` version could declare its post card and reference it in both places, ending the duplicate. A ref
cannot be partially overridden (copy the part to change it). The update notice needs a client build; older clients
just don't show it.
