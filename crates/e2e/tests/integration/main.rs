//! The integration tests of `e2e`, linked as one test binary: one
//! binary per file made CI spend most of its build time linking.
//! Add a new test file as a module here.

mod collections;
mod custom_apps;
mod notifications;
mod suite;
mod upstream_faults;
