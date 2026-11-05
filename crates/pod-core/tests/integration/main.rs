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
