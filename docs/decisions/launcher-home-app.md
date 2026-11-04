# The Construct can be the phone's launcher: a `launcher` app made of layouts

Date: 2026-11-04. Related: D34, D40, D50, [layout-features](layout-features.md),
[custom-apps-and-data-sources](custom-apps-and-data-sources.md), [mcp-connector](mcp-connector.md).

Status: prototype. Everything here is real code, but it is a first cut to try on a phone before committing to it.

The goal is fewer client installs, and The Construct as the home screen. The app list should be dynamic: the pod
decides what home shows and how, and the client keeps working offline. Most chrome changes needed a client
build so far (the menu, Settings, badges). A launcher would multiply that unless the home screen itself is a
layout.

Decision: the home screen is an ordinary app, `launcher` (adapter `home`, `crates/app-home`), whose screens are layout
files (`layouts/launcher/NNNN.json`). It is not called `home` because staging still keeps the removed counter app's
`layouts/home` history (D39), and a new `home/0001.json` would stop pods from starting.

The client learns only generic vocabulary, as named layout features:

- `backdrop`: `core://Backdrop`, the dark sketchpad surface (a perspective grid tilted back) with a scrolling
  `content` and a fixed `footer`, drawn in a dark theme.
- `clock`: `core://Clock` (time or date).
- `gestures`: `core://Swipe` (swipe up / down) and `core://Tappable.on_long_press`.
- `on_appear`: `core://OnAppear`, an intent dispatched when it appears. Inside a `core://Conditional` on a status
  it acts when the status changes, e.g. opening an app once it is built.
- `autofocus`: `core://TextField.autofocus`, so swiping up lands in the search box.
- `app_open`: `action.app.open {app_id}`, opening another pod app from a layout.
- `device_apps`: the client fills the query `device:apps` with the apps installed on the device
  (`device://apps/<package>`), and `action.device.launch {package}` starts one. The list never leaves the
  device: the client merges it into each snapshot, like staged attachments. The engine and the pod never see it.
- `hidden_chrome`: `chrome: "hidden"` on a workspace or screen draws only the layout (no title, bar, menu or
  bell), and `action.shell.open {panel}` opens the menu or the inbox from it.

What the `launcher` app does with them:

- **Home:** the clock, the pinned apps as a plain text list (Construct apps marked ◇), requests being built,
  and a + button.
- **Drawer:** swipe up to search every app, Construct apps first, then the phone's.
- **Pinning:** long-press an app in the drawer to pin it; long-press a pinned one to unpin it. Pins live on the
  pod (`home_pins`), so every device sees them.

**+ → Claude builds an app.** The + screen sends the description to `POST /v1/home/requests`. The pod runs a
tool-use loop with the model (`CONSTRUCT_INFER_*`, model `CONSTRUCT_HOME_BUILDER_MODEL`). The model's tools are
the pod's own MCP tools, called in-process and limited to a safe subset: read, create collections, `create_app`,
and update only the app this run created. The pod updates the request's status as it goes and tells the device
with `event: data` (D50). The request screen draws the request as a one-item list, so when its status becomes
`ready`, a `core://OnAppear` opens the new app. Without a model key, and on temporary pods (the pod setting
`home.build=0`), the request fails with a sentence saying why. Each pod may ask for 20 apps a day.

**Offline:** the phone draws the newest home it has: the pod's current version, else the engine's cached
workspace (layout, pins and the app list), else a copy of the newest `layouts/launcher` file bundled into the
client at build time. So home draws on first launch with no network. The pod can still change home at any time by
serving a new version. Phone apps come
from the device, so they always launch. A request made offline waits in the outbox, and its screen says so.

On Android the app can be chosen as the launcher (a `HOME` intent filter), and it starts on `launcher`. Back at the
root of another app returns home.

Weighed:

- **A native launcher screen in the client:** every change to it would need a build again.
- **Putting the device's app list in the engine or on the pod:** the pod has no use for it, and it is private.
- **Running the builder on the device:** the pod already holds the model key and the validated write path.

Not done yet:

- Icons for phone apps; widgets; notification dots.
- A long-press menu (app info, uninstall, versions) instead of pin/unpin.
- Ordering pins by hand.
- Offering the drawer's search text to + ("Make 'plant…' with Claude").
- A cached app list for the native drawer.
- A device check of the Android launcher.
