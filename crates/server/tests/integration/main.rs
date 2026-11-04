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
