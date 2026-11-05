# Adapters ship server components; Pixelfed's signed-out screen is one

Date: 2026-11-05. Related: [plumbing-and-porcelain](plumbing-and-porcelain.md),
[blueprints-and-home](blueprints-and-home.md), D45, issue #135.

Pixelfed's signed-out screen (a sentence, "Sign in with Pixelfed", and a server box when other servers are allowed)
was a `UiNode` tree built in Rust by the adapter. An agent couldn't read it with `get_component`, restyle it or reuse
it, and only `crates/pod-core/components/` could hold server components, which would put an app's piece in the core
(rule 1: the core knows no app).

Decision: an adapter ships server components in its own crate, `crates/app-*/components/<name>/NNNN.json`, embedded
by its build script (`generate_component_files` in `pod-core/build_support`) and returned by
`AppAdapter::component_files`. They are `core:<name>` like the core's: recorded at start, append-only, listed by
`list_components`, copyable to `pod:`. A name shipped by two crates stops the start. An adapter's screens (the
signed-out screen and a stale sign-in's `Hydration::StaleSession` or `Hydration::Screen`) may hold `core://Use`
nodes: the core expands them (leniently, as when serving a layout) and leaves the workspace a workspace, so the app
draws its chrome as before.

Pixelfed's signed-out screen is now a workspace whose slot is a use of `core:pixelfed_sign_in` (`server`,
`instance_url`, `other_servers`, `placeholder`). The expansion is exactly the tree the adapter built before, with the
same `action.auth.login` intents, so every client draws it as before and no version negotiation is needed; a test
checks it node for node.

Not done yet: the component's buttons carry the pod's Pixelfed auth routes (`action.auth.login`), not
`action.device sign_in` with a provider from `local:providers`. A layout using it must pass `instance_url`; moving to
the device command (which needs the device's provider list loaded) would make it argument-free but changes what old
clients can do, so it is left for a later version of the component.
