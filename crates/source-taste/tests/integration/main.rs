//! The `taste` source against a fake taste service (decision
//! `taste-data-source`).

// The fake's helpers hand back a refusal as an axum `Response`.
#![allow(clippy::result_large_err)]

mod fake;
mod history;
mod layouts;
mod routes;
