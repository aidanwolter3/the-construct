# The headless simulator

`construct-sim` (`crates/construct-sim`) runs a small script against a session of the Rust engine
(`construct-render`, [rendering.md](rendering.md)), draws each `shot` on the CPU and writes PNGs. There is no window,
no GPU, and a run takes a second or two, so it works in CI and cloud sessions. It is the project's UI check:
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
build (`core_layout_<id>_page.json`), the other bundles to `open-layout`, screen data paths from
`pixelfed_screen_data.json` and `events_screen_data.json`, and notifications and their channels from
`notifications.json` (each created `age_seconds` before the run). Writes are accepted and kept by nobody: the device
keeps its optimistic change, but a created entity (a posted comment) doesn't come back from the pod. `offline` makes
the fake pod unreachable: `local://connection` says Offline, loads fail, and a write is refused and undone (the Rust
clients have no outbox). Images whose path ends in `/storage/<file>` or `/v1/pixelfed/media/<file>` come from
`fake-pixelfed/fixtures/media/`; every other URL fails at once, so shots are the same on every run.

Default bundles: `pixelfed_sample_no_login` (`pixelfed_sample_bundle.json`), `pixelfed` (`pixelfed_feed_bundle.json`,
the default workspace), `events` (`events_bundle.json`) and `c-kitchen2dm` (`custom_kitchen_bundle.json`); any other
bundle with `--fixture`. `theme_home_bundle.json`, `theme_groceries_bundle.json` and `theme_log_bundle.json` are
hand-written pages drawn only with theme tokens (a home, a shopping list, a log), for checking both modes.
`text_styles_bundle.json` (workspace `text_styles`, the `text-styles` scenario) is a hand-written page of text
weights, capitals and line limits, and conditionals with `not` and `count`. `buttons_bundle.json` (workspace `buttons`, the `buttons` scenario) is a hand-written run screen with every `core://Button` style and size side by side (decision `one-button-node`). `agent_report` (`agent_report_bundle.json`,
the `agent-report` scenario) is a hand-written page of made-up agent runs whose texts are Markdown (`core://Text`
`markdown`): two-line previews, a question and a whole report with every block the reader knows. `screen_sizes` (`screen_sizes_bundle.json`)
is a hand-written recipes page using the size vocabulary (decision `screen-sizes`), for `screen tablet` and `screen desktop`. `device_versions` (`device_versions_bundle.json`, the `device-versions` scenario) is a Server health page with made-up
devices as the `monitor` source's `devices` feed serves them: a `core://Bars` chart of devices per day by build (one block per build, coloured from the theme's chart colours, decision `chart-colours`), kind
chips and a tap that picks a day (decision `device-versions`). `server_health` and `server_health_off` (`server_health_bundle.json`, `server_health_off_bundle.json`, the `server-health` and `server-health-off` scenarios) are the `monitor` source's `server_health` core layout over made-up data (`crates/server/tests/data/simulator/server_health_data.json`), its devices a grid of devices by day (feature `bars_rows`, decision `device-day-grid`) with a few mid-day updates: with both feeds (one check warning), and as a pod without monitoring gets it (the devices and the source's `error 403`); the fixtures test writes both. `taste` (`taste_bundle.json`, with `taste_screen_data.json` for its screens, the `taste` scenario) is the `taste` source's core layout over made-up answers of a taste service (`crates/server/tests/data/simulator/taste_data.json`), made into entities by the source's own code; the fixtures test writes both. `list_trailing` (`list_trailing_bundle.json`, the `list-trailing` scenario) is a hand-written grid of made-up workout sessions ending with a dashed add tile (feature `list_trailing`), and an empty grid that still draws one. Every bundle's layout is what a pod serves this build: a workspace as the `core://Page` it
expands to. `UPDATE_FIXTURES=1 cargo test -p server --test integration fixtures::` writes the Pixelfed ones and the
core layouts from the pod (and the hand-written ones from their sources in `crates/server/tests/data/simulator/`; the
test fails when one is stale); edit the source and rerun it.

**Live mode** (`--base-url <pod>`) runs a live session (`remote::PodClient`) on a pod, starting on `home`. The session
token or pod code comes from `CONSTRUCT_POD_TOKEN` (never a flag or a file); unset, the pod answers its sign-in
screen. The token goes only to the pod's own origin: a data path a layout writes as an absolute URL elsewhere is
fetched without it. Signing in (`action.auth.session`, e.g. `click Try a temporary pod` on staging) and Leave pod reopen the
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
| `--now` | this machine's clock | The device's wall clock, stopped at `YYYY-MM-DDTHH:MM`, for dates (`format`, `date` conditionals) that don't change from run to run. |
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
| `press <target>` / `release` | Holds a finger down on the node's centre (a `core://Tappable` there is drawn pressed) until `release` or a `click`; `release` also lets go of a card `drag-card` holds. |
| `type <target> <text>` | Replaces a text field's text (`field:<name>`, `tag:textfield:<name>`, its placeholder, or the field's name). |
| `swipe-left <target>` / `swipe-right <target>` | Scrolls a chip row (`tag:chips:<field>`) by 60% of the screen's width, else throws a `core://SwipeCard` off that way (its intent runs), else pages a carousel, else runs the `core://Gesture`'s `on_swipe_left` / `on_swipe_right` there. |
| `drag-card <points> <target>` | Moves the `core://SwipeCard` there sideways by that much (negative: left) and holds it, for a shot of it mid-drag; `release` lets go (past its threshold it flies off and its intent runs, else it springs back). |
| `swipe-down <target>` / `swipe-up <target>` | A flick: closes a sheet from its handle (`tag:sheet-handle`), else runs the gesture's `on_swipe_down` / `on_swipe_up`. |
| `drag-down <points> <target>` | Drags a sheet's handle down and lets go without a fling (it closes past a quarter of its height), else scrolls the view there up by that much. |
| `scroll-to <target>` | Scrolls until the node is on screen. |
| `hover <target>` | Rests the mouse over the node's centre (a `core://Tappable` there is drawn hovered, as on desktop and the web) until another `hover`. |
| `back` | A system Back press. |
| `wait <ms>` / `idle` | Keeps drawing for that long / waits until no load or write is out. |
| `assert-text <text>` / `assert-no-text <text>` | Some node's text or content description contains (or doesn't contain) the text; retries 2 s. |
| `wait-text <ms> <text>` | `assert-text` with a longer timeout. |
| `assert-link <text>` | An `action.link.open` opened a link containing the text. |
| `wait-images [ms]` | Waits until no on-screen image is loading (default 10 s). |
| `screen <WxH or preset>` | A new screen size; the session carries over. |
| `mode dark` / `mode light` | The system's light or dark mode (light at start); `mode dark; shot dark` shows a screen in dark mode. |
| `insets <top> <right> <bottom> <left>` | The space a phone's system bars cover, in points (none at start): `insets 36 0 24 0` shows what each screen draws under the status and navigation bars (decision `system-bars-follow-the-page`). |
| `clock <YYYY-MM-DDTHH:MM>` | Stops the device's wall clock there (as `--now`), to show a screen before and after a `date` conditional changes: `clock 2026-11-16T08:59; shot before; clock 2026-11-16T09:00; shot after`. |
| `offline` / `online` | Fixture mode: the fake pod becomes unreachable / reachable. |
| `open-layout <id> [push\|sheet\|panel]` | Presents a layout (`action.navigation.open {layout}`) and waits for its data. |
| `open-settings` | Presents the Settings layout as a side panel. |
| `open-inbox` | Presents the inbox layout as a sheet. |
| `notify <layout> <channel> <title>` | Fixture mode: a notification arrives (the inbox and its count update; the fake pod serves it in `local:notifications` and as a row of `collection:notifications`). |
| `toggle-channel <layout>.<channel>` | Flips a notification channel (`toggle_channel`; fixture mode). |
| `construct <file>` | Fixture mode: the pod's construct moves to the one in the file (`{"layouts": {key: page}, "reach": {key: [keys]}, "data"?}`, decision `content-addressed-construct`): changed layouts are drawn again where they are open (after a focused field or a touch ends), and a stack entry the one below no longer reaches closes. Start with `--workspace home` (`construct_home_bundle.json`); `construct_v1.json` to `construct_v4.json` are an example; `construct_anchor_above.json` adds rows above a scrolled list (the `scroll-anchor` scenario); `construct_panels.json` is a home whose inline meals layout opens panels on a desktop window (the `panels` scenario, decision `wide-screen-panels`). |
| `pick <path> [<path>…]` | Queues files (relative to the repository root) for the next media picker tap, at most its `max`; none queued is a cancelled pick. Fixture mode: the fake pod stages each file where it is (`sim-<n>` ids). |
| `publish-version <note>` | Parsed, but fails: the engine always shows a layout's latest version. |

A step whose tap or intent fails (a load the pod refuses, an intent nothing in this mode runs) fails.

A `<target>` is `tag:<id>`, a single word, or `"quoted text"`. `tag:<id>` is a semantic id
([rendering.md](rendering.md#semantic-ids)): exact, else the first id containing it; the old Compose
simulator's tags `textfield:<f>` and `textfield-send:<f>` stand for `field:<f>` and `send:<f>`. Text is matched exactly (tappable
nodes first), then as a substring, against texts and content descriptions. The engine leaves what is off screen out
of the semantic tree, so a target not on screen is searched for along each chip row on screen, then by scrolling the
screen's largest scrolling view; a chip past its row's edge is scrolled sideways into view.

Common ids: a node's own `tag` (`like:<post uri>`, `comment:<post uri>`, `carousel:<post uri>`), `field:<name>`,
`send:<name>`, `clear:<name>`, `<field>=<value>` for a chip or dock tab, `list:<query key>`, `layout:<id>`, `page:<carousel>`, and
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
