# `event: data` says what changed; the engine reloads only that

Date: 2026-11-08. Related: D50, [collections](collections.md),
[custom-apps-and-data-sources](custom-apps-and-data-sources.md),
[held-local-sets-and-list-item-reuse](held-local-sets-and-list-item-reuse.md).

Tapping a favorites toggle in a groceries list wrote one `view_prefs` row; the POST came back in 40 ms, but the
pod then sent `event: data {workspace_id}` to every device, the writer included, and the engine reloaded the
workspace and every data path it had loaded this run, one request at a time under the `net` mutex: about 30
requests and 8 seconds on a phone, most of them for screens long closed (recipes, meals, events), four copies of a
2051-entity layout, and a deleted layout that answered 404 every time.

Decision:

- **The pod says what changed.** A Collections write (a row create, update or delete, an action, a model's answer
  to a computed field) publishes one `event: data` for everything its transaction changed, the computes that
  followed included (`follow::settle` records every changed row):
  `{workspace_id, source, changes: [{collection, rows}], uri_prefixes, query_prefixes}`. `changes` is the
  source's own account (shown in logs, for people); `uri_prefixes` and `query_prefixes` are what clients match:
  data changed if an entity URI starts with a `uri_prefix`, or a query key is a `query_prefix` or starts with one
  followed by `:`. Collections sends `collections://<c>/` and `collection:<c>`. `pod-core` carries the scope
  (`DataScope`, `Pod::publish_data_scoped`) without knowing what it means, and the engine compares strings
  without knowing Collections.
- **The writer's device isn't told,** when the write changed nothing it didn't send: the stream of the device named
  by the write request's `x-construct-device` skips the event. Its engine already shows the write (optimistic
  value, held local sets) and reloads a create's list itself when the create is accepted. When a computed field of
  the row or a follow-up in another collection changed, the writer is told like everyone else. A write without a
  device header (MCP, agents, the server's own work) reaches every device.
- **Old clients keep working:** they read `workspace_id` and reload everything, as before. Events that don't know
  what changed (Pixelfed, filler runs, definition changes, an event lost to a lagging stream) have no prefixes.
- **The engine reloads what matches, on screen first.** Each cached data path (and each `layout:<id>`) is matched
  by the entity URIs and queries of its last answer; the workspace by its last fetch this run (before that it
  always matches). The renderer tells the engine what each frame draws, topmost first
  (`Loader::visible` → `ConstructEngine::set_visible`): every drawn entry's layout, its screen's data path, and
  the layouts drawn inline in it. Matching keys on screen reload now in that order; the others are marked stale
  and reload as soon as they are on screen again (Back to a screen still in the stack); opening a screen loads it
  anyway. Until the client says what's on screen, everything that matches reloads.
- **A few at a time:** reloads run at most 4 requests at once (`reload::PARALLEL_LOADS`). The `net` lock is now a
  read/write lock: workspace fetches and data loads share it, outbox POSTs take it alone, so a load still never
  races a flush.
- **404 drops the path:** a data path or layout that answers 404 is dropped from the cache (memory and the store),
  so no later event reloads it.
- An event without prefixes still reloads everything, 4 at a time.
- **A reconnect catches up** (2026-11-11). An `event: data` sent while the stream was down (the app in the
  background, a network drop) is lost: tasks set to done through the connector stayed in their old section until
  opened. Every reconnect after the stream's first connection with the same session counts as one unscoped event
  that respects what's on screen (`Inner::catch_up_data`): what's on screen reloads now, every other cached path and
  layout is marked stale. When the app returns to the foreground (`Loader::foregrounded` →
  `ConstructEngine::app_foregrounded`, from the window's `resumed`), a stream waiting out its backoff reconnects at
  once, and one silent for over 20 s (`events::FOREGROUND_SILENCE`, past the pod's 15 s keepalive: it died while the
  app was suspended) is reopened; either way the reconnect catches up.

Not done: other sources (Pixelfed posts, Events fillers) still send unscoped events; a filler run could name its
collection. The renderer reports a sheet's or panel's own layout and the entries under it, but not which list
items are scrolled into view; a long list is one data path either way.
