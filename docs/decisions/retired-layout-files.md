# Old layout files can be deleted

Date: 2026-11-04. Related: D32, D37, [named-vocabulary](named-vocabulary.md),
[data-next-to-its-code](data-next-to-its-code.md).

Layouts as code (D32) kept every version an app ever had as a file, `layouts/<app>/0001.json` onward, and the pod
refused to start unless an app's files ran without gaps from `0001`. By now most of those files were dead weight:
twenty files across four apps, of which only the newest per app is served to current clients. The others use
vocabulary that was since removed (`pixelfed://` widgets, the create's `dismiss`), or were superseded, and the
clients that could only draw them are old builds of our own. Adapters also had to list "legacy files" that may
hold removed node types (D37), only so those old files kept parsing.

Two ways were considered. Renumbering each app's newest file to `0001` breaks every pod that recorded the
history: its `0001` would no longer match. Wiping the recorded history with a migration would empty the menu's
version history and cut custom apps off from the built-in version they were copied from.

Decision: old files may be deleted from the repo, oldest first; the newest file of each app stays.

- An app's files must run without gaps from its lowest file, which may be above `0001`
  (`pod_core::layouts::parse_files`). Files are still never edited or renumbered.
- A pod that recorded the deleted files keeps them in its history and serves them as before. Recorded files
  numbered below the build's lowest file are retired: the start logs no warning for them. A deployed file missing
  from the build above that (a rollback deploy) still warns.
- A new pod (a temporary pod, a test) starts from the oldest file present, as its version 1, so version numbers
  and file numbers differ there. `layouts::builtin` is that oldest file.
- A client that can't draw the oldest file left gets `409 no_compatible_version` on a new pod. On existing pods it
  is served the recorded version it draws, as before.
- The legacy-file allowance is gone (`AppAdapter::legacy_files`, `StaticAdapter::with_legacy_files`): no file in
  the build holds a removed node type. Recorded versions that do are still served as stored.
- Tests that need a history of several versions build it themselves (`crates/server/tests/apps.rs`
  `feed_history`, synthetic sources in `crates/app-pixelfed/tests/layout_files.rs`) instead of relying on old
  files.

First retirement, this decision's change: every app kept only its newest file (`pixelfed/0010`,
`pixelfed_sample_no_login/0002`, `recipes/0005`, `events/0003`; the ids are the apps' current ones, decision
[rename-apps-and-folders](rename-apps-and-folders.md)). The deleted files are in git history.
