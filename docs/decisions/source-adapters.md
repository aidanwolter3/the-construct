# Adapters are sources: `source-*` crates and the `Source` trait

Date: 2026-11-16. Related: D40, [layouts-not-apps](layouts-not-apps.md), [notification-owner](notification-owner.md),
[named-migrations](named-migrations.md), [rename-apps-and-folders](rename-apps-and-folders.md). Issues #187, #295.

Since [layouts-not-apps](layouts-not-apps.md) there are no apps at runtime, but the word stayed in the code that
plugs data into the pod: the adapter crates were `crates/app-*`, their trait `AppAdapter`, Pixelfed's OAuth client
registrations lived in `pixelfed_apps`, a notification and a device's channel choice named their layout `app_id`,
and the MCP server still answered `create_app`, `delete_app`, `list_apps` and `rename_app`.

Issue #295 (a content-addressed construct) replaces the HTTP endpoints, the layout store and the layout vocabulary
outright, so renaming those first would mean two migrations and two client breaks. They stay as they are here and
move with #295: `/v1/apps`, `/v1/custom-apps`, `/v1/workspaces/<id>` and their response fields; the tables
`custom_apps` and `app_versions`; `core://Ref`'s `app`, `action.device`'s `app`, `local:apps` / `local://apps`;
client-core's `list_apps` / `app_versions` / catalog naming; construct-render's `apps.json`; and `AppInfo` with
`Source::apps`, which describe that catalog.

Decision:

- **Adapters are sources.** The crates are `crates/source-pixelfed`, `source-recipes`, `source-collections`,
  `source-events`, `source-monitor`, `source-agents` and `source-shell` (libraries `source_*`), and the trait is
  `pod_core::Source`. A source is data with its routes and jacks; the blueprints it ships stay in its crate
  (`crates/source-*/layouts/<blueprint>/`) until #295 moves them. A pod records layout files by `<app>/NNNN`, not
  by path, so the move changes nothing stored.
- **`pixelfed_clients`.** Pixelfed's adapter step `pixelfed_clients` renames the table `pixelfed_apps` (it holds
  OAuth client registrations). Its rows move with it. A sealed `client_secret` keeps its sealing context,
  `pixelfed_apps.client_secret:<instance> <redirect_uri>`, so secrets sealed before still open and the developer's
  Pixelfed sign-in survives.
- **Notifications name a layout.** The core step `notification_layouts` renames the column `app_id` to `layout` in
  `notifications` and `device_channel_prefs`; values, the unique key and devices' choices stay. Channels are still
  keyed `<layout>.<channel>` (Android channel ids, `PUT /v1/devices/{id}/channels`), with the same values.
- **The wire keeps the old names too.** Clients already installed read `app_id`, so the pod sends both names until
  #295's old system is removed, and then `app_id` goes:
  - a channel (`GET /v1/notifications/channels`): `layout` and `app_id`, the same value;
  - a notification (list, `notification` event, Web Push): `channel_layout` and `app_id`, the same value. A
    notification's `layout` already means the layout to present it in (decision `layouts-not-apps`), and
    installed clients read it that way, so the channel's layout takes the name `channel_layout` there.

  Readers take either name (`protocol::channel_layout_in`, `protocol::notification_layout_in`; the Rust types parse
  both). `local://channels/{layout}/{id}` keeps its shape, and the device entities are the wire objects, so layouts
  may read `$item.layout` (channels) on any client once the pod sends it; the `settings` blueprint keeps reading
  `$item.app_id` until #295, so no feature is needed yet (CONTRIBUTING rule 3 applies when `app_id` is removed).
  `unread_by_app`, the `app` filter of `GET /v1/notifications` and `toggle_channel`'s `app` keep their names and go
  with #295.
- **MCP.** The aliases `create_app`, `delete_app`, `list_apps` and `rename_app` are gone (an agent calls
  `create_layout`, `delete_layout`, `list_layouts`, `rename_layout`). `list_parts` takes `layout` and answers
  `{layouts: [{layout, title, version, parts}]}`; `rebase_context` answers `id`. `copied_from {app, version}` is the
  layout store's and stays for #295.

Consequences: no client build is needed for this change; installed desktop and Android builds keep working, since
every name they read is still sent. A build from after it reads the new names first. A rollback of the server to a
build from before it would not find `pixelfed_clients` or the `layout` columns: roll forward instead.
