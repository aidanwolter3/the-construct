//! Fetching web pages and reading structured data out of them (decisions
//! `watcher` and `collection-fillers`; the Events app and Collections' fetch fillers).
//!
//! - [`fetch`]: a conditional `GET` with a size cap, and the URL check that
//!   keeps fetches on public websites.
//! - [`html`]: a page's readable text and its JSON-LD blocks, without a DOM.
//! - [`items`]: the generic extractors, each turning a fetched body into a
//!   list of JSON items: `json`, `jsonld`, `rss`, `ics`, `embedded-json` and
//!   `selectors`.
//! - [`path`]: the small path language that picks the items out of a
//!   document and a value out of each item (`results[*].votes`).

pub mod events;
pub mod fetch;
pub mod html;
pub mod items;
pub mod path;
pub mod when;
