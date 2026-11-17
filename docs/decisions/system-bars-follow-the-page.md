# The Android app draws under its system bars, which show each screen's own page

Date: 2026-11-17. Related: [android-native-bridge](android-native-bridge.md), [rust-clients](rust-clients.md).

The Android app painted its status and navigation bars one fixed colour, white in light mode and the engine's dark
page in dark mode (`ConstructActivity.applySystemBars` and the theme XML). A pod whose theme or layout draws another
page colour (a cream `${local://theme#page}`) showed a white band above and below every screen, and no colour baked
into the app can follow a pod's theme, which changes without an app update. Considered: a theme token the app reads
for the bars (only covers the theme, not a layout's own `background`, and is one more thing a layout author has to
know), and the platform's "edge to edge" mode with the bars' colour taken from what is drawn.

Decision: the app draws edge to edge, under transparent bars with no contrast scrim, and the renderer decides what is
under them. The engine already pads the layout's root by the window insets; for the top full-screen layout the scene
also paints that layout's page colour into the insets, edge by edge (`Laid::edge_colors`: the opaque, square box drawn
last across the whole edge, outside anything that scrolls), falling back to the theme's background. Sheets and panels
draw as they did. The scene reports the colours under the status bar and the navigation bar (`Scene::bars`); the window
tells the platform when either changes between light and dark (`Platform::system_bars`), and Android sets the bars'
icons to match (`ConstructActivity.setLightBars`, a forwarder per the bridge decision). The night mode no longer
decides the icons; the theme XML's are only the launch's, until the first frame.

Consequences: any colour a pod's layout or theme paints at the screen's edges shows under the bars with no app update.
Boxes inside a scrolling view don't count (a bar's colour doesn't flicker as rows pass the edge), and a side panel
leaves the status bar the colour of the dimmed page under it. Desktop and the simulator have no bars and ignore the
call.

Later the same day: sheets, shades (an overlay from the top) and panels kept the theme's surface, so a layout on the
page colour showed surface-coloured bands at its handle strip and under the bars. They are now drawn on their own
layout's page colour (where it meets the handle strip, or the top for a panel), and the handle is a tint of it.
