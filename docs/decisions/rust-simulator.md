# The headless simulator runs on the Rust engine

Date: 2026-11-07. Related: D8, [rust-render-engine](rust-render-engine.md),
[data-next-to-its-code](data-next-to-its-code.md).

Layout and UI changes were checked only with `:desktopApp:simulate`, which draws the Compose app (D8). Once the
clients cut over to the Rust engine (issue #166), nothing would check layouts as the clients draw them. The engine
had the pieces (`pod_shots`, a scripted live session to PNGs; `render_bundle`; the golden tests) but no fixtures,
assertions or CI.

Decision: the simulator is `construct-sim`, a binary in its own crate (`crates/construct-sim`) over
`construct-render`'s `Session`, drawn by the CPU rasterizer so CI needs no GPU and no JVM:

- **The same script language** as the Compose simulator (`shot`, `click`, `type`, `assert-text`, `wait-text`,
  `open-layout`, `back`, …) and the same output (`NN-<name>.png`, `steps.log`, `steps.json`, `NN-FAILED.png`), so
  scripts and habits carry over. `tag:<id>` targets name the engine's semantic ids; the Compose tags
  `textfield:<f>` and `textfield-send:<f>` map to `field:<f>` and `send:<f>`.
- **Fixture mode** is a fake pod (`fixture::FixturePod`, a `Loader`) over the bundles the Compose simulator used,
  moved to `crates/construct-sim/data/`, next to the code that now reads them; the Compose simulator reads them
  there until it is deleted. The fake pod accepts writes without keeping them (the device keeps its optimistic
  change), and offline it refuses loads and writes as an unreachable pod does: the Rust clients have no outbox.
- **Live mode** is `remote::PodClient` against any pod, with the token from `CONSTRUCT_POD_TOKEN` (never a flag
  or a file), and signing in and leaving the pod reopen the session as a client does.
- `pod_shots` is gone: `construct-sim --base-url` does what it did.

Not yet: `pick` (the engine opens no media picker) and `publish-version` (it always shows a layout's latest
version) fail with a message; the Compose simulator keeps them until #166.
