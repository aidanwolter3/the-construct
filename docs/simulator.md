# The headless simulator

`construct-sim` (`crates/construct-sim`) runs a small script against a session of the Rust engine
(`construct-render`, [rendering.md](rendering.md)), draws each `shot` on the CPU and writes PNGs. There is no window,
no GPU and no JVM, and a run takes a second or two, so it works in CI and cloud sessions. It is the project's UI check:
there are no emulator or on-device tests. Decision: [`rust-simulator`](decisions/rust-simulator.md).

```bash
cargo run -p construct-sim -- --workspace pixelfed_sample_no_login --screen phone --out /tmp/sim \
  --script "shot feed; click Like; assert-text Unlike; shot liked; offline; shot offline"
scripts/sim_scenarios.sh /tmp/sim       # the scenarios CI screenshots, one folder each
```

The output directory holds `<NN>-<name>.png` (`NN` the step's number), `steps.log` and `steps.json` (one entry per
step: `index`, `command`, `arg`, `status`, `message`, `file`, `elapsed_ms`). The run stops at the first failing step
and saves the screen as `<NN>-FAILED.png`; a failed text check lists the texts on screen.

CI runs `scripts/sim_scenarios.sh` in the Rust job and uploads the PNGs as the `simulator` artifact; those are the
screenshots PRs embed. The same scenarios, with stronger checks, are `crates/construct-sim/tests/integration/scenarios.rs`.

## Modes

**Fixture mode** (the default) needs no server. A fake pod (`fixture::FixturePod`) serves bundles from
`crates/construct-sim/data/`: the start layout's bundle, the shell's Settings and inbox as a pod serves them to this
build (`blueprint_<id>_page.json`), the other bundles to `open-layout`, screen data paths from
`pixelfed_screen_data.json` and `events_screen_data.json`, and notifications and their channels from
`notifications.json` (each created `age_seconds` before the run). Writes are accepted and kept by nobody: the device
keeps its optimistic change, but a created entity (a posted comment) doesn't come back from the pod. `offline` makes
the fake pod unreachable: `local://connection` says Offline, loads fail, and a write is refused and undone (the Rust
clients have no outbox). Images whose path ends in `/storage/<file>` or `/v1/pixelfed/media/<file>` come from
`fake-pixelfed/fixtures/media/`; every other URL fails at once, so shots are the same on every run.

Default bundles: `pixelfed_sample_no_login` (`pixelfed_sample_bundle.json`), `pixelfed` (`pixelfed_feed_bundle.json`,
the default workspace), `events` (`events_bundle.json`) and `c-kitchen2dm` (`custom_kitchen_bundle.json`); any other
bundle with `--fixture`. `theme_home_bundle.json`, `theme_groceries_bundle.json` and `theme_log_bundle.json` are
hand-written pages drawn only with theme tokens (a home, a shopping list, a log), for checking both modes. Every bundle's layout is what a pod serves this build: a workspace as the `core://Page` it
expands to. `UPDATE_FIXTURES=1 cargo test -p server --test integration fixtures::` writes the Pixelfed ones and the
blueprints from the pod (and the hand-written ones from their sources in `crates/server/tests/data/simulator/`; the
test fails when one is stale); edit the source and rerun it.

**Live mode** (`--base-url <pod>`) runs a live session (`remote::PodClient`) on a pod, starting on `home`. The session
token or pod code comes from `CONSTRUCT_POD_TOKEN` (never a flag or a file); unset, the pod answers its sign-in
screen. Signing in (`action.auth.session`, e.g. `click Try a temporary pod` on staging) and Leave pod reopen the
session as a client does:

```bash
cargo run -p construct-sim -- --base-url https://the-construct-pod.fly.dev --out /tmp/sim \
  --script "click Try a temporary pod; wait-text 20000 Notifications; shot home; open-settings; shot settings"
```

`offline`, `online` and `notify` are fixture-only.

## Flags

| Flag | Default | Meaning |
|---|---|---|
| `--workspace` | `pixelfed`; live: `home` | The layout at start; in fixture mode it picks the default bundle. |
| `--screen` | `phone` | `phone` (390 × 844 points), `tablet` (820 × 1180), `desktop` (1280 × 800), or `WxH` in points. |
| `--density` | per screen | Pixels per point: phone 3, tablet 2, desktop 2, `WxH` 2. |
| `--script` | `shot initial` | The script. |
| `--script-file` | | The script, from a file (instead of `--script`). |
| `--fixture` | per workspace | Fixture mode: the start layout's bundle (`{workspace, entities, queries}`). |
| `--base-url` | | Live mode against this pod. |
| `--timeout-ms` | `10000` | Live mode: how long to wait for the first load. |
| `--out` | `target/sim/<timestamp>` | Output directory. |

Flags take `--flag value` or `--flag=value`. Relative paths resolve against the working directory, else the
repository root. Exit codes: 0 every step passed, 1 a step failed, 2 bad flags or script.

## Script commands

Commands are separated by `;` or newlines; entries starting with `#` are comments (up to the next `;`). The first
word is the command and the rest of the entry, as written, its argument: `click Count: 2` clicks "Count: 2", and
`click "Sign in"` looks for the quote marks too (only `type` and `pick` read quotes).

| Command | Effect |
|---|---|
| `shot <name>` | Writes `<NN>-<name>.png` (waits up to 5 s for on-screen images first). |
| `click <target>` | Taps the node's centre, scrolling it into view first. |
| `type <target> <text>` | Replaces a text field's text (`field:<name>`, `tag:textfield:<name>`, its placeholder, or the field's name). |
| `swipe-left <target>` / `swipe-right <target>` | Pages a carousel, else runs the `core://Gesture`'s `on_swipe_left` / `on_swipe_right` there. |
| `swipe-down <target>` / `swipe-up <target>` | A flick: closes a sheet from its handle (`tag:sheet-handle`), else runs the gesture's `on_swipe_down` / `on_swipe_up`. |
| `drag-down <points> <target>` | Drags a sheet's handle down and lets go without a fling (it closes past a quarter of its height), else scrolls the view there up by that much. |
| `scroll-to <target>` | Scrolls until the node is on screen. |
| `back` | A system Back press. |
| `wait <ms>` / `idle` | Keeps drawing for that long / waits until no load or write is out. |
| `assert-text <text>` / `assert-no-text <text>` | Some node's text or content description contains (or doesn't contain) the text; retries 2 s. |
| `wait-text <ms> <text>` | `assert-text` with a longer timeout. |
| `assert-link <text>` | An `action.link.open` opened a link containing the text. |
| `wait-images [ms]` | Waits until no on-screen image is loading (default 10 s). |
| `screen <WxH or preset>` | A new screen size; the session carries over. |
| `mode dark` / `mode light` | The system's light or dark mode (light at start); `mode dark; shot dark` shows a screen in dark mode. |
| `offline` / `online` | Fixture mode: the fake pod becomes unreachable / reachable. |
| `open-layout <id> [push\|sheet\|panel]` | Presents a layout (`action.navigation.open {layout}`) and waits for its data. |
| `open-settings` | Presents the Settings layout as a side panel. |
| `open-inbox` | Presents the inbox layout as a sheet. |
| `pin-version <layout> <n or latest>` | `pin_version` / `follow_latest` (live mode; the engine still shows the latest version). |
| `notify <app_id> <channel> <title>` | Fixture mode: a notification arrives (the inbox and its count update). |
| `toggle-channel <app>.<channel>` | Flips a notification channel (`toggle_channel`; fixture mode). |
| `pick <path> [<path>…]` | Queues files (relative to the repository root) for the next media picker tap, at most its `max`; none queued is a cancelled pick. Fixture mode: the fake pod stages each file where it is (`sim-<n>` ids). |
| `publish-version <note>` | Parsed, but fails: the engine always shows a layout's latest version. The Compose simulator below still runs it. |

A step whose tap or intent fails (a load the pod refuses, an intent nothing in this mode runs) fails.

A `<target>` is `tag:<id>`, a single word, or `"quoted text"`. `tag:<id>` is a semantic id
([rendering.md](rendering.md#semantic-ids)): exact, else the first id containing it; the Compose tags
`textfield:<f>` and `textfield-send:<f>` stand for `field:<f>` and `send:<f>`. Text is matched exactly (tappable
nodes first), then as a substring, against texts and content descriptions. The engine leaves what is off screen out
of the semantic tree, so a target not on screen is searched for by scrolling the screen's largest scrolling view.

Common ids: a node's own `tag` (`like:<post uri>`, `comment:<post uri>`, `carousel:<post uri>`), `field:<name>`,
`send:<name>`, `<field>=<value>` for a chip or dock tab, `list:<query key>`, `layout:<id>`, `page:<carousel>`, and
`confirm:ok` / `confirm:cancel` for an `action.confirm` dialog's buttons.

Examples:

```bash
# Layouts over the root: pushed, a side panel, a sheet; Back pops each
--script "open-layout events; shot events; back; open-settings; shot settings; back; open-inbox; shot inbox; back"
# A post's comments, then typing one
--script 'click tag:comment:pixelfed://posts/781254316723458049; scroll-to Which lens?; type tag:textfield:comment Golden light!; shot typed'
# Notifications
--script 'notify pixelfed likes Bob liked your post; open-inbox; shot inbox; click @bob commented on your post; shot thread'
```

## The Compose simulator (until #166)

`:desktopApp:simulate` draws the Compose app (`ConstructApp`) offscreen with the same script language, for as long
as Compose is the product; issue #166 deletes it. It reads the same fixtures from `crates/construct-sim/data/`.

```bash
cd app && ./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=pixelfed_sample_no_login -Pscreen=phone \
  -Pout=/tmp/sim -Pscript="shot feed; click Like; assert-text Unlike; shot liked; offline; shot offline"
```

Its flags are `-P` properties: `-Pworkspace`, `-Pscreen`, `-Pdensity`, `-Pscript`, `-PscriptFile`, `-Pfixture`,
`-Pout` as above (relative paths resolve against the repository root), plus `-Ptoday=YYYY-MM-DD` (the date
formatting draws against), `-PbaseUrl` (live mode, the real Rust engine through JNA, with a fresh temporary
storage unless `-Pstorage`), `-PtimeoutMs`, and `-PpixelfedLogin=<login>:<password>` (live sign-in through a real
Pixelfed's web form; against the fake Pixelfed the scripted browser approves at once). Its fake engine
(`FakeEngineBridge`) models the outbox (`offline` queues writes, `online` flushes them), keeps created posts and
comments, serves a fake app catalog (Pixelfed v1..v3, Pixelfed sample v1..v2) for `publish-version` and
`pin-version`, and runs `pick`.

Its targets are Compose test tags: layout nodes' `tag`s as above, `screen:<id>`, `screen-back`, `layout-back`,
`textfield:<field>`, `textfield-send:<field>`, `list-empty`, `sheet-handle`, `sheet:<layout>`, the inbox's
`notification-row:<id>`, `inbox-mark-all`, `channel:<app>.<channel>`, `channel-switch:<app>.<channel>`, and the
version picker's `version-picker`, `version-latest`, `version-pinned`, `version-<n>`.
