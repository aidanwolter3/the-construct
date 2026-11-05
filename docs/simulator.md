# The headless simulator

`:desktopApp:simulate` renders the real shared UI (`ConstructApp`) to an offscreen screen, runs a small script
against it and writes PNG screenshots. There is no window and a run takes a few seconds, so it works in CI and cloud
sessions. It is the project's UI check: there are no emulator or on-device tests.

Run it from `app` with a JDK 21 (see [CONTRIBUTING.md](../CONTRIBUTING.md#setup)):

```bash
./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_sample_no_login -Pscreen=phone -Pout=/tmp/sim \
  -Pscript="shot feed; click Like; assert-text Unlike; shot liked; offline; shot offline"
```

The output directory holds the PNGs, `steps.log` and `steps.json` (one entry per step: `index`, `command`, `arg`,
`status`, `message`, `file`, `elapsed_ms`). The run stops at the first failing step and saves the screen as
`<NN>-FAILED.png`.

## Modes

**Fixture mode** (the default) needs no server and, with `-PskipRustBuild`, no Rust build. A fake engine
(`FakeEngineBridge`) loads a bundle from `app/desktopApp/src/sim/resources/`, applies taps with the same expression subset as the engine,
models the outbox (`offline` queues, `online` flushes), and serves a fake app catalog (Pixelfed v1..v3, Pixelfed sample
v1..v2) and notifications (`app/desktopApp/src/sim/resources/notifications.json`). Images whose path ends in `/storage/<file>` or
`/v1/pixelfed/media/<file>` come from `fake-pixelfed/fixtures/media/`; every other URL fails at once, so shots are
deterministic. Screen data comes from `app/desktopApp/src/sim/resources/pixelfed_screen_data.json`, generated from the
pod with `UPDATE_FIXTURES=1 cargo test -p server --test integration fixtures::` (the test fails when it is stale).

**Live mode** (`-PbaseUrl=<server>`) runs the real Rust engine against a server, with a fresh temporary storage
directory unless `-Pstorage` is given. Images load over HTTP. `offline`, `online`, `publish-version` and `notify` are
fixture-only.

<a id="live-mode"></a>**Sign-in in live mode:** the engine gets an in-memory session store and a "browser" that
follows the authorize URL with `auto=1`, which the fake Pixelfed approves at once:

```bash
./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed -PbaseUrl=http://127.0.0.1:8080 \
  -Pscript="click Sign in with Pixelfed; open-settings; wait-text 20000 @constructdemo; back; wait-text 20000 Alice Lumen; wait-images 20000; shot feed"
```

For a real Pixelfed, pass `-PpixelfedLogin=<login>:<password>`: the browser then logs in through Pixelfed's web form
and approves the consent page. Against staging, start with `click Try a temporary pod`.

## Flags

| Flag | Default | Meaning |
|---|---|---|
| `-Pworkspace` | `pixelfed` | The app at start; in fixture mode it picks the default fixture. |
| `-Pscreen` | `phone` | `phone` (390x844 dp), `tablet` (820x1180), `desktop` (1280x800), or `WxH` in dp. |
| `-Pdensity` | per screen | Pixels per dp: phone 3, tablet 2, desktop 2, `WxH` 2. |
| `-Ptoday` | the real date | `YYYY-MM-DD` that date formatting draws against, for repeatable shots. |
| `-Pscript` | `shot initial` | The script. |
| `-PscriptFile` | | The script, from a file (instead of `-Pscript`). |
| `-Pmode` | `fixture` | `fixture` or `live`; `-PbaseUrl` selects live. |
| `-Pfixture` | per workspace | The bundle in fixture mode: `pixelfed_sample_no_login` uses `crates/app-pixelfed/sample/bundle.json`, `pixelfed` uses `app/desktopApp/src/sim/resources/pixelfed_feed_bundle.json`; other apps need this flag. |
| `-PbaseUrl` | staging | The server in live mode. |
| `-PtimeoutMs` | `10000` | Live mode: how long to wait for the first load. |
| `-Pout` | `app/desktopApp/build/sim/<timestamp>` | Output directory. |
| `-PpixelfedLogin` | | Live mode: `<login>:<password>` for a real Pixelfed. |
| `-Pstorage` | a fresh temp dir | Live mode: the engine's storage, kept after the run (to test restarts). |

Relative paths resolve against the repository root. Exit codes: 0 success, 1 a failed step, 2 bad flags or script.
An unknown `-P` property is ignored with a warning.

## Script commands

Commands are separated by `;` or newlines; entries starting with `#` are comments (up to the next `;`).

| Command | Effect |
|---|---|
| `shot <name>` | Writes `<NN>-<name>.png` (waits up to 5 s for on-screen images first). |
| `click <target>` | Clicks the node, scrolling it into view first. |
| `type <target> <text>` | Replaces a text field's text. |
| `pick <path> [<path>…]` | Queues files for the next photo picker. |
| `swipe-left <target>` / `swipe-right <target>` | Swipes a node, e.g. a carousel. |
| `swipe-down <target>` | A quick downward flick from the node's center (a fling), e.g. `swipe-down tag:sheet-handle` closes a sheet. |
| `drag-down <dp> <target>` | Drags slowly down by `dp` from the node's center and lets go without a fling, e.g. `drag-down 300 tag:sheet-handle` (a sheet's handle) or `tag:sheet:<layout>` (its content). |
| `scroll-to <target>` | Scrolls until the node is visible. |
| `back` | A system Back press. |
| `wait <ms>` / `idle` | Sleeps and advances the UI clock / waits for idle. |
| `assert-text <text>` / `assert-no-text <text>` | Some node's text or content description contains (or doesn't contain) the text; retries 2 s. |
| `wait-text <ms> <text>` | `assert-text` with a longer timeout. |
| `assert-link <text>` | An `action.link.open` opened a link containing the text. |
| `wait-images [ms]` | Waits until no image is loading. |
| `screen <WxH or preset>` | Restarts the renderer at a new size; engine state carries over. |
| `offline` / `online` | Fixture mode: flips connectivity. |
| `publish-version <note>` | Fixture mode: publishes a new version of the root layout. |
| `open-layout <id> [push\|sheet\|panel]` | Presents a layout (`action.navigation.open {layout}`) and waits until it is drawn. |
| `open-settings` | Presents the Settings layout as a side panel. |
| `open-inbox` | Presents the inbox layout as a sheet. |
| `pin-version <layout> <n or latest>` | Shows a version of a layout on this device (`pin_version`), or the latest again. |
| `notify <app_id> <channel> <title>` | Fixture mode: a live notification arrives. |
| `toggle-channel <app>.<channel>` | Flips a channel switch (`toggle_channel`). |

`back` pops the top of the back stack (a presented layout or a screen). In fixture mode the fake pod serves the
`app-shell` blueprints (`settings`, `inbox`) as a pod serves them to this build, as plumbing
(`app/desktopApp/src/sim/resources/blueprint_<id>_page.json`; `blueprint_<id>.json` is the workspace older builds
get, written by the same `fixtures::` test), and the fixtures' layouts to `open-layout`.

A `<target>` is `tag:<testTag>` (exact, else the first tag containing it), a single word, or `"quoted text"`: exact
text, then exact content description (clickable nodes first), then substrings.

Examples:

```bash
# Layouts over the root: pushed, a side panel, a sheet; Back pops each
-Pscript="open-layout events; shot events; back; open-settings; shot settings; back; open-inbox; shot inbox; back"
# Comments: open, write, send
-Pscript='click tag:comment:pixelfed://posts/781254316723458049; wait-text 3000 Which lens?; type tag:textfield:comment Golden light!; click tag:textfield-send:comment; wait-text 3000 just now; shot sent'
# Notifications
-Pscript='notify pixelfed_sample_no_login likes Bob liked your post; open-inbox; shot inbox; click @bob commented on your post; shot comments'
# Posting a photo
-Pscript='click tag:new-post; pick fake-pixelfed/fixtures/media/carousel-1.png; click tag:media-picker-add:photos; click Share; shot posting'
```

## Test tags

- **Layout nodes:** `screen:<id>`, `screen-back`, `screen-loading`, `screen-error`, `screen-retry`,
  `textfield:<field>`, `textfield-send:<field>`, `list-empty`; a node's own `tag` (e.g. `carousel:<post uri>`,
  `like:<post uri>`, `bookmark:<post uri>`, `comment:<post uri>`).
- **Menu:** `menu-button`, `menu-badge`, `menu-drawer`, `menu-close`, `menu-back`, `app-<id>`, `app-history-<id>`,
  `app-more-<id>`, `app-hide-<id>`, `app-delete-<id>`, `app-custom-<id>`, `apps-show-hidden`, `apps-error`,
  `app-error`, `app-action-error`, `app-gone`, `app-updates-<id>`, `app-updates-dismiss-<id>`, `delete-app-dialog`,
  `delete-app-confirm`, `delete-app-cancel`, `notice`, `notice-dismiss`, `menu-newer`, `menu-newer-latest`.
- **Version picker:** `version-picker`, `version-latest`, `version-pinned`, `version-<n>`, `versions-more`,
  `versions-error`, `version-error`.
- **Settings:** `menu-settings`, `settings`, `menu-connection`, `menu-account`, `sign-in-<id>`, `sign-in-detail`,
  `sign-ins-error`, `menu-sign-in`, `menu-sign-out`, `menu-leave-pod`, `menu-notifications`, `menu-server`,
  `server-<key>`, `server-custom`, `server-custom-url`, `server-custom-hint`, `server-custom-use`, `provider-<id>`,
  `provider-server-<id>`, `provider-server-go-<id>`.
- **Notifications:** `inbox-button`, `inbox-badge`, `inbox`, `notification-row:<id>`, `inbox-mark-all`,
  `inbox-close`, `notification-settings`, `channel:<app>.<channel>`, `channel-switch:<app>.<channel>`.
