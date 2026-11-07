# The Rust clients serve screen readers through AccessKit, from the semantic tree

Date: 2026-11-07. Related: [rust-render-engine](rust-render-engine.md).

The Rust clients (`construct-viewer` and the NativeActivity app) draw every pixel themselves, so screen readers saw
an empty window. Compose gets this from its semantics tree; the engine already builds one per frame
(`SemanticNode`: id, type, bounds, text, what a tap does, selected), which hit-testing and agents use. Writing each
platform's accessibility API (NSAccessibility, UI Automation, AT-SPI, Android's node provider) by hand was the other
option.

Decision: the semantic tree is turned into an AccessKit tree (`construct-render/src/accessibility.rs`, plain data,
always built and tested without a window), and the desktop window hands it to `accesskit_winit`, which serves
VoiceOver, Narrator and Orca. Roles come from node types; a tappable's descendants fold into it, as Compose merges a
clickable's semantics; a scrim hides what is under it. Requests come back as the session's own steps (a tap by
semantic id, a scroll, a field's value), so a screen reader does what a finger does. AccessKit node ids are stable
hashes of the semantic ids. A text field's placeholder is kept in the tree (`SemanticNode::hint`) to name it.

Android is not served. `accesskit_android` injects a Java class into the view (a prebuilt dex, `embedded-dex`, or
the app's own Java) and looks the view up as a GameActivity's, while the app is a NativeActivity written without
Java or Kotlin. Which way to go is the developer's call (issue #172).
