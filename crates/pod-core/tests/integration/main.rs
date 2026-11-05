//! The integration tests of `pod-core`, linked as one test binary: one
//! binary per file made CI spend most of its build time linking.
//! Add a new test file as a module here.

mod adapters;
mod blueprints;
mod custom_apps;
mod notifications;
mod parts;
mod sources;
mod sse;
mod uploads;

/// Every feature this build draws but `page`: such a client gets a
/// `core://Workspace` as it is, not the plumbing it stands for (decision
/// `plumbing-and-porcelain`).
pub fn workspace_features() -> String {
    protocol::FEATURES
        .iter()
        .filter(|f| **f != "page")
        .copied()
        .collect::<Vec<_>>()
        .join(",")
}
