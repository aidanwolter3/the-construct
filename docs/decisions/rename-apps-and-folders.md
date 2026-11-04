# Apps and folders take clearer names; layouts live in their adapter crate

Date: 2026-11-04. Related: D32, [data-next-to-its-code](data-next-to-its-code.md),
[retired-layout-files](retired-layout-files.md), [collection-fillers](collection-fillers.md).

Several names no longer said what they were. The Events app kept the id `watcher` (and its crate `app-watcher`)
after the Watcher it replaced was gone. `pixelfed_home` read as "the main app" when it meant "your home feed", and
`pixelfed_demo` didn't say it was the one that needs no account. The clients' Gradle project was `kmp-app`. And
each app's layout files sat in a root `layouts/` folder, away from the adapter that serves them, the same problem
decision `data-next-to-its-code` fixed for data files.

Keeping the old ids as internal names was considered: renaming an app id breaks installed clients (they cache and
open apps by id) and touches what pods store. The project has one user and one set of clients, so the break is
cheap now and only grows; the old names would keep confusing people reading the code.

Decision: rename, everywhere.

| Was | Now |
|---|---|
| app `watcher`, crate `crates/app-watcher` | app `events`, crate `crates/app-events` (adapter `events`) |
| app `pixelfed_home` | app `pixelfed` |
| app and source `pixelfed_demo` ("Pixelfed demo"), `POST /v1/demo/ack` | `pixelfed_sample_no_login` ("Pixelfed sample"), `POST /v1/pixelfed_sample_no_login/ack` |
| `crates/app-pixelfed/demo/bundle.json` | `crates/app-pixelfed/sample/bundle.json` |
| `layouts/<app>/NNNN.json` | `crates/<adapter>/layouts/<app>/NNNN.json` |
| `kmp-app/` (Gradle root project `kmp-app`) | `app/` (root project `app`) |

- **Layout files** move into their adapter crate, one folder per app (`app-pixelfed` has two). The build script
  reads the crate's own `layouts/` (`crates/pod-core/build_support/layout_files.rs`), and `LayoutSource::dir`
  carries the folder for messages. A pod records files by `<app>/NNNN`, not by path, so moving them changes
  nothing it stored.
- **Each renamed app gets a new file** with the new root id, numbered after its old one (`events/0004`,
  `pixelfed/0011`, `pixelfed_sample_no_login/0003`); the old files were already retired. A new pod starts from it.
- **Stored data moves with a migration**, run by each adapter (the core still knows no app):
  `pod_core::store::rename_app_on` renames an app's layout history (every version's root id and file key, so the
  history, pinned versions and `copied_from` numbers stay valid), its notifications, devices' channel choices, the
  hidden list, and custom apps' `core://Ref` parts and `copied_from`. `rename_source_on` renames a source in custom
  apps' feeds. On an existing pod the renamed old file key (`events/0003`) is below the build's lowest file, so it
  is retired, and the new file appends as the next version. Pixelfed's migration 4 renames both of its apps; the
  Events adapter's migration 1 renames `watcher`.
- **The Watcher's remains go.** Its tables (`watcher_*`), its import into `events` (every pod ran it long ago),
  the `watcher.imported` setting and its routes and feeds that answered 410 for its old layouts are removed. The
  `events` collection records the app it belongs to; `app_events::events::ensure` updates it to `events`.
- **Clients need an update**: their default app was `pixelfed_home`, and the Events app's slot id changed with its
  layout. A client's cache of the old ids is simply unused.
- **Older decisions** mention the old names. Contrary to the usual rule, they were updated to the new app ids so a
  search finds them; the old Watcher's own tables, tools and routes keep their names there, since they were never
  renamed, only removed.
