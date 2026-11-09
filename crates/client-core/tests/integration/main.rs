//! The integration tests of `client-core`, linked as one test binary: one
//! binary per file made CI spend most of its build time linking.
//! Add a new test file as a module here.

mod also;
mod apps;
mod attachments;
mod auth;
mod data_events;
mod engine;
mod events;
mod notifications;
mod outbox;
mod pod_host;
