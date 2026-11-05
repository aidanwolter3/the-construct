//! The integration tests of `server`, linked as one test binary: one
//! binary per file made CI spend most of its build time linking.
//! Add a new test file as a module here.

mod apps;
mod fault_hook;
mod fixtures;
mod host;
mod host_limits;
mod mcp;
mod monitor;
mod open_pod;
mod pixelfed;
mod routes;
mod shutdown;

/// Every feature this build draws but `page`: such a client gets a
/// `core://Workspace` as it is, not the plumbing it stands for (decision
/// `plumbing-and-porcelain`). Tests of the plumbing send them all.
pub fn workspace_features() -> String {
    protocol::FEATURES
        .iter()
        .filter(|f| **f != "page")
        .copied()
        .collect::<Vec<_>>()
        .join(",")
}
