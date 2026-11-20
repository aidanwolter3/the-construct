# Device versions: one build id, devices recorded per pod, watches that wake agents

Date: 2026-11-19. Related: [server-monitoring](server-monitoring.md), [named-vocabulary](named-vocabulary.md),
[notification-owner](notification-owner.md), [agents-runs](agents-runs.md), [web-client](web-client.md).

The developer wants to see which of their devices (a phone, a Mac, a few browsers) reach the pod on which build,
day by day, and their agents need to know when the phone has a build that draws something new, so they can change
layouts the phone shows. Nothing said which build a client ran: the apps' version lived only in their packaging
(`CONSTRUCT_VERSION_NAME`), the server and the web client had none, and the web client sent no device id.

Decision:

- **One build id** for the server, the web client and the apps, which are built from the same commits:
  `<version>+<commit time, UTC, YYYYMMDDHHMMSS>.<short hash>[.dirty]` (`scripts/build_id.sh`), compiled in from
  `CONSTRUCT_BUILD` (`protocol::build()`). CI's deploy passes it to the image as a build argument (the image has
  no `.git`), the release workflow to the apps, and `android_native.sh`, `macos_app.sh` and `web_build.sh` make it
  when it isn't set. The commit time orders builds, so a build from a branch the server never ran still compares;
  a build without it (`+dev`, a plain `cargo run`) has no order. The server's own build is the latest; a device
  is on it, behind it by the server builds the pod ran since its own, or ahead of it (an app flashed before its
  deploy landed). Options considered: the release version name (only tags have one; staging deploys every merge)
  and a run number (differs between the workflows that build the server and the apps).
- **Every engine request says who it is**: the existing per-install `x-construct-device`, now also sent by the web
  client (a random id kept in `localStorage`, so each browser is one device), `x-construct-build: <platform>
  <build id>` and `x-construct-device-name` (the phone's model, the Mac's name, `Chrome on macOS`). The simulator
  says `sim`. MCP calls and internal requests carry none and are not devices.
- **The pod records them** (`pod_core::devices`, tables `client_devices`, `client_days`, `server_builds`,
  `device_watches`) in its own database, so a pod sees only its own devices. A ping costs no write: a middleware
  remembers each device and writes what changed at once and the rest at most once a minute. Days are in the pod's
  time zone; devices unseen for 60 days and days older than 90 are dropped.
- **The `monitor` source's `devices` feed** shows them (the rows are the pod's own, so unlike `overview` it needs no
  operator switch): the devices with how their build stands, and each device's build per day for 14 days, today's
  bar every device at its latest build (hatched when not seen today). Each build has a clearly different hue (a
  shaded ramp of one hue proved too hard to tell apart): the server's blue, the six newest behind it orange, aqua,
  yellow, magenta, green and violet (newest first, an order whose neighbours stay apart for colour-blind eyes and
  that reads on the light and the dark card alike), newer ones red, older ones and unknown grey. A device that a new
  one of the same kind and name replaced (first seen after it was last seen: a browser whose stored id was lost, an
  app installed again) is folded into the new one, its days counted as the new one's, so one browser is one row.
- **A new plumbing node, `core://Bars`** (feature `bars`), draws it: items grouped into bars by a field, each a
  segment as tall as its value and coloured by a binding, filtered like a list, and a tap that sets a form value as
  choice chips do. No existing node could size a shape from data. It is drawn from the renderer's own boxes and
  taps, so every client (desktop, Android, web, simulator, previews) has it at once.
- **MCP**: `list_devices` (with each device's features, so an agent checks for the feature it wants to use rather
  than comparing versions), `rename_device`, and `watch_device`. MCP notifications reach only a connected client,
  and an agent waiting on a phone isn't connected, so the pod does the waking: a watch names a device (or a kind)
  and waits for a feature or a build at least as new as one; when a ping meets it the pod runs, once, a source
  action as the owner (e.g. the Agents source's reopen of a run, with a message) or sends a notification on the
  "Device updates" channel of the layout that shows the `devices` feed.

Limits: an old app shows as "Version unknown" (it sends its device id but no build) until it is updated; its row
updates in place then. A browser's private window or cleared storage is a new device (the feed shows the two as
one when the old one stopped before the new one started; the records and `list_devices` keep both). Device names are hints
the device sends; the user's own name (`rename_device`) wins. "Behind by N" counts only server builds this pod was
opened under. Not done: renaming a device from the app (the MCP tool only).
