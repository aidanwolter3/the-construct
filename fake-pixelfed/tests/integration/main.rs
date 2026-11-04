//! The integration tests of `fake-pixelfed`, linked as one test binary: one
//! binary per file made CI spend most of its build time linking.
//! Add a new test file as a module here.

mod admin;
mod api;
mod comments;
mod notifications;
mod posts;
