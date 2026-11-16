# The construct is the only layout system: every pod is switched, the old endpoints and tables are gone

Date: 2026-11-16. Related: [content-addressed-construct](content-addressed-construct.md),
[source-adapters](source-adapters.md), [layouts-not-apps](layouts-not-apps.md), [layout-parts](layout-parts.md),
[named-vocabulary](named-vocabulary.md), [blueprints-and-home](blueprints-and-home.md),
[device-layout-and-inbox-commands](device-layout-and-inbox-commands.md), [named-migrations](named-migrations.md),
[retired-layout-files](retired-layout-files.md). Issues #300, #187 (the grep gate).

[content-addressed-construct](content-addressed-construct.md) made the construct opt-in per pod and kept the old
layout system beside it: two stores (`custom_apps`, `app_versions`, `layout_blobs`, `components` beside `objects`
and `names`), two wire paths (`/v1/workspaces`, `/v1/apps`, `/v1/custom-apps` beside `/v1/construct`), two
vocabularies (`core://Ref`, versions, pins and hidden layouts beside references and names) and an engine that spoke
both. Its rollout said the old system goes once the developer had switched their own pod and was happy with it.
They have.

Considered:

- **A. Keep the old system for pods nobody switched.** Every change would still be written twice, and the
  installed clients' fallback would keep the old paths alive forever.
- **B. Remove it, and refuse to open a pod that isn't switched.** A pod nobody switched (a temporary pod, a pod
  the developer forgot) would stop working at the deploy.
- **C. Remove it, and switch every pod still on it when it opens, with the same conversion and a logged dry run.**
  Taken.

Decision: **the construct is the only layout system. A pod still on the old system is switched when it first opens
on this build; then the old tables are dropped, and only the construct's paths answer.**

## The startup switch

- When a pod opens and isn't switched, the pod converts its old tables as `switch_construct` did (the conversion of
  [content-addressed-construct](content-addressed-construct.md), unchanged), runs the dry run first and logs its
  report: `info` when every layout draws the same both ways, `warn` with each differing layout and its differences
  otherwise. **It switches either way**: the differences are logged, not a reason to stay on a system that is
  gone. The conversion and the switch are one transaction.
- A new pod is seeded on the construct directly, with the home a switch of a new pod gave before.
- Then the named migration that drops the old tables runs, on every pod: switched by the developer, switched at
  this open, or new.
- The conversion is migration code: it lives under `crates/pod-core/src/migrations/`, reads the old tables with
  plain SQL, and runs only for a pod that isn't switched. MCP `switch_construct`, `server pods switch` and the
  revert are gone. `server pods list` shows each pod's system.
- `@core/` names (blueprints, their parts, server and source components) are moved to this build's files on every
  open, read from the files themselves rather than from the old tables. A name's history is the client's fallback
  as before: a client is sealed the newest element of a name it can draw.

## What is removed, and what replaces it

| Removed | Replaced by |
|---|---|
| `GET /v1/workspaces/{id}` | `GET /v1/construct`, `POST /v1/objects`, `GET /v1/construct/data/{id}`, `GET /v1/construct/names/{name}` |
| `GET /v1/workspaces/{id}/events` (home's stream) | `GET /v1/construct/events`: `construct`, `data`, `notification`, `notifications_read` |
| `GET /v1/apps` (layout titles and icons) | each element's `title` and `icon`, read from the objects the device has |
| `GET /v1/apps/{id}/versions[/{n}]` | a name's history on the pod (MCP `get_layout`, `restore_version`); no versions on the device |
| `PUT /v1/apps/{id}/hidden`, `/v1/custom-apps/*` | the MCP layout tools (they write the construct) |
| `GET /v1/construct` `409 not_switched` | (every pod is switched) |

- **Tables** `custom_apps`, `app_versions`, `layout_blobs`, `components`, and the pod settings `apps.hidden`,
  `updates.dismissed` and the old conversion markers, by a named migration. The engine's `pinned_versions` table, by
  a client store migration.
- **Pod code**: `pod_core::custom` (custom-app writes, `ref_update`, `updates_from`, `dismiss_update`,
  `rebase_context`, `copied_from`), the old serving and versioning of `layouts` and `parts`, the `components`
  table's code, the old branches of the MCP tools, and the MCP tools `set_hidden`, `dismiss_update`,
  `rebase_context` and `switch_construct`.
- **Vocabulary**: `core://Ref {app, part}` is no longer accepted (the startup conversion still reads it). The
  feature `device_layout_controls` (`hide_layout`, `unhide_layout`, `delete_layout`) moves to `features/removed/`;
  the device commands `show_versions`, `pin_version` and `follow_latest`, and the local data `local:layouts`,
  `local:all_layouts`, `local:apps` and `local:versions` are gone with it. Settings is a new blueprint file without
  its Layouts section and versions screen, and its older files are deleted
  ([retired-layout-files](retired-layout-files.md)).
- **Kept as authoring spellings**: `core://Use {component}` and `layout: <id>` on `core://Layout` and
  `action.navigation.open` are still accepted on write and normalized to `ref`. They never reach a client, and
  `layout: <key>` stays the plumbing the engine materializes for the renderer.
- **Engine**: the layout-system mode and its `/v1/workspaces` fallback, pinned versions, the apps catalog
  (`list_apps`, `app_versions`, `apps.json`), and Settings' Layouts section and versions screen.
- **Names** ([source-adapters](source-adapters.md) left them for this change): notifications and channels no longer
  carry `app_id` (`channel_layout` and `layout` do); `unread_by_app` is `unread_by_layout`; the notifications
  filter `app` is `layout`; `toggle_channel` takes `layout`; `AppInfo` and `Source::apps` describe blueprints.
- **The grep gate** of #187: `scripts/check_old_names.sh` (run in CI) fails when
  `custom_apps|app_versions|core://Ref|pin_version|local:layouts` appears under `crates/` outside migration code and
  `crates/protocol/features/removed/`.

## Consequences

- **Installed clients break at the deploy.** A build from before this change draws a switched pod over
  `/v1/workspaces/home/events` and `/v1/apps`, which are gone: new desktop and Android builds are needed, flashed
  right after the deploy. A new build can't draw a pod on an older server either (it calls only the construct's
  paths).
- The developer can no longer revert a pod to the old system; the old tables are dropped. Name history (MCP
  `restore_version`) is the way back to an earlier layout.
- A pod whose dry run reported differences draws the converted layouts; its log says which and how.
