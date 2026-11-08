//! The integration tests of `app-collections`, linked as one test binary: one
//! binary per file made CI spend most of its build time linking.
//! Add a new test file as a module here.

mod collections;
mod computed;
mod fillers;
mod follow;
mod lookup;
mod reminders;
