# The app no longer draws core://Workspace: the pod serves it as the page it expands to

Date: 2026-11-06. Related: [plumbing-and-porcelain](plumbing-and-porcelain.md),
[named-vocabulary](named-vocabulary.md), [retired-layout-files](retired-layout-files.md), issues #124 and #126.

Decision `plumbing-and-porcelain` made `core://Workspace` porcelain: a pod serves it to an app that draws `page`,
`scroll` and `nav_data` as a `core://Page` expanded through `core:workspace` and `core:screen`. The app still carried
its own workspace and screen chrome for older builds (the title bar, the pushed layout's Back and inset, the screen
bar with spinner, error row and footer, `fillsScreen`). Every desktop and Android build in use now sends `page`, so
that code is retired, as that decision planned.

The usual way to retire vocabulary (move the feature to `features/removed/`) would have retired every version that
uses it, and every built-in layout and many of the pods' own are workspaces. A workspace isn't gone, though: the pod
still expands it.

Decision:

- `workspace`, `screens` and `title_scrolls` move to `features/removed/`, so new builds don't send them. The Rust
  types stay, and old builds that send them still get the workspace as stored.
- They are **expanded features** (`protocol::EXPANDED_FEATURES`): removed for clients, but still written. A layout
  write, a component definition and the home conversion refuse only the other removed features
  (`protocol::is_retired`), so agents and blueprint files may keep using `core://Workspace`; `describe_vocabulary`
  lists it under porcelain.
- For a client that draws the plumbing, a version's compatibility ignores those three features
  (`AppVersion::compatibility`), and serving checks the version's expansion instead, as it already did for
  component uses (`layouts::active_for`). A client that doesn't draw the plumbing and lacks them sees the version as
  retired.
- The app draws a raw workspace as a one-line notice ("This page needs a newer pod."), which only a pod that can't
  expand it would send. A workspace screen (no `root`) draws nothing, and route lookup only knows page screens.
- The simulator's fixture bundles hold what a pod serves this build (the expansion), written by the server's
  `fixtures::` test from the pod and from workspace sources in `crates/server/tests/data/simulator/`. The golden test
  that compared the app's workspace drawing with the expansion is replaced by a check that each shell blueprint draws
  as a page.

Consequences: new builds need the pod's expansion for every workspace; screens drawn through `core:screen` differ
from the old app chrome by a few pixels in the bar (noted in `plumbing-and-porcelain`). The engine (`client-core`)
still parses workspaces, which it caches and hands over unchanged.
