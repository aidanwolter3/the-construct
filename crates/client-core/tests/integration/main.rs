//! The integration tests of `client-core`, linked as one test binary: one
//! binary per file made CI spend most of its build time linking.
//! Add a new test file as a module here.

mod also;
// Ported with the engine to the construct (issue #300): they drive the old
// layout system's server APIs, which are gone.
// mod apps;
mod attachments;
mod auth;
// mod construct;
mod data_events;
mod engine;
mod events;
mod notifications;
mod outbox;
mod pod_host;
