//! The integration tests of `source-collections`, linked as one test binary: one
//! binary per file made CI spend most of its build time linking.
//! Add a new test file as a module here.

// Tests call their own local servers, not hosts a user picked.
#![allow(clippy::disallowed_methods)]

mod bulk;
mod collections;
mod computed;
mod fillers;
mod follow;
mod inbox;
mod lookup;
mod mcp_fillers;
mod primitives;
mod provenance;
mod reminders;
mod render;
mod research;
mod row_id;
mod views;
mod web;
