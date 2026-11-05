# A source's notifications belong to the one layout that shows it

Date: 2026-11-05. Related: [layouts-not-apps](layouts-not-apps.md), [server-monitoring-alerts](server-monitoring-alerts.md),
[collection-reminders](collection-reminders.md), [collection-fillers](collection-fillers.md). Issue #116.

Since [layouts-not-apps](layouts-not-apps.md), home owns every feed and every layout reports home's feeds. The
adapters that notify for the pod's layouts still picked layouts by "has this source among its feeds", which now
matches all of them: Settings → Notifications listed "Server warnings" once per layout and a monitor warning sent one
notification per layout, and Collections reminders and alerts went to the first match, which is home (listed first).

Considered: making the channel the pod's rather than a layout's (Settings and the inbox group by layout, and a tap
must open something that shows the data); keeping a layout's own feed list beside home's (two lists to keep in step,
and layouts made since #111 declare none).

Decision:

- **The owner of a feed** (`custom::feed_owners(store, catalog, source)`, `feed -> layout`) is the oldest live
  layout whose latest version, parts expanded, binds the feed by one of its query keys; else the oldest that binds an
  entity of the source by URI (`${scheme://…}`); else home. Home is the last choice in both steps. A feed no layout
  declares (home's feeds) has no owner. It is worked out when needed, from the layouts as they are now, so it
  follows edits: it is never stored.
- **Monitoring:** the `server_warnings` channel and each warning's notification belong to the owner of `monitor`
  `overview`: one channel, one notification.
- **Collections:** a reminder or alert with its own `app` (while that layout lives) keeps it; otherwise it belongs to
  the owner of the collection's feed. A system collection's alerts keep their built-in app's channel.
- `AppAdapter::custom_notification_channels` takes the catalog too, so adapters can expand parts.
- `describe_collection` and `list_collections` `used_by` list the layouts whose latest version binds the collection,
  not every layout that has the feed.

Consequences: no wire or client change. A channel can move to another layout when the layouts change (the device's
on/off choice is stored per layout and channel id, so a moved channel starts at its default). Two layouts that show
the same source still get one channel, on the oldest.
