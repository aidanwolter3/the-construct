//! The integration tests of `source-pixelfed`, linked as one test binary: one
//! binary per file made CI spend most of its build time linking.
//! Add a new test file as a module here.

// Tests call their own local servers, not hosts a user picked.
#![allow(clippy::disallowed_methods)]

mod layout_files;
mod notifications;
