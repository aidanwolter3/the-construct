# Agent previews read the pod like the owner's device, and never write

Date: 2026-11-11. Related: [rust-render-engine](rust-render-engine.md),
[custom-apps-and-data-sources](custom-apps-and-data-sources.md).

`render_layout_preview` drew a layout's bundle and home's feeds, but its session had no `Loader`: a screen's data
path came back as `action.data.load` not sent (the Recipes recipe screen drew empty ingredient and step lists),
the device data the clients build (`local:layouts`, `local://inbox`, …) was missing (home drew blank), and a
screen could only be reached by finding a row and tapping it. Checking a layout meant rebuilding parts of it with
literal values, which hides the bugs being looked for.

Considered: keeping previews loader-less and adding more preloaded data per case (each screen, each local list);
or giving previews the full engine. The first never catches up with what layouts bind; the second brings an
outbox and an event stream a preview must not have.

Decision: a preview's session gets a read-only `Loader` (`pod_core::preview::Reader`) that asks the pod's own
router as the owner, so a preview behaves as the owner's device with their data, minus anything that writes:

- Device data is built by the same code the clients use, moved out of the `remote` feature into
  `construct_render::device_data`. Facts only a device knows get defaults (online, an Android phone with
  notifications allowed).
- A screen's data path and `action.data.load` are read only when the path matches a data path a data source
  declares (`SourceManifest::data_paths`): those are the GETs that only return entities. Any other path is
  refused, so a draft can't make a preview read an arbitrary route (the event stream, a sign-in callback).
- Layouts a tap opens and a layout's versions are read; a picked version stays in the preview.
- Every write a `Loader` could take (`send`, `take`, `auth`, `open_session`, `device_command`) keeps its default,
  so the session reports it as not sent, as before.

The preview also opens a screen directly (`target_uri`, `params`), binds `$item` and adds made-up entities and
lists (`item`, `entities`, `queries`), draws a long screen whole (`full_height`) or scrolled to a node
(`scroll_to`), and lists the node tree as `text` (a short tap summary, the default), `full` or `none`.

Limits: reads happen while the preview is drawn, so a slow data path makes the preview slow (each read times out
after 20 s). Images are not downloaded by the pod's renderer, as before.
