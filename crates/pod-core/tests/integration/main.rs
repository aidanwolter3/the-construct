//! The integration tests of `pod-core`, linked as one test binary: one
//! binary per file made CI spend most of its build time linking.
//! Add a new test file as a module here.

mod adapters;
mod core_layouts;
mod jacks;
mod notifications;
mod sources;
mod sse;
mod uploads;

/// The features of a build from before issue #126: every feature this
/// build draws but `page`, plus the workspace's own. Such a client gets a
/// `core://Workspace` as it is, not the plumbing it stands for (decision
/// `plumbing-and-porcelain`).
pub fn workspace_features() -> String {
    protocol::FEATURES
        .iter()
        .filter(|f| **f != "page")
        .chain(&protocol::EXPANDED_FEATURES)
        .copied()
        .collect::<Vec<_>>()
        .join(",")
}
