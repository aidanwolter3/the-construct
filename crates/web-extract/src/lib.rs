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
//!
//! It is also the pod's **web module** (decision `web-and-agent-modules`):
//!
//! - [`hosts`]: one policy per host, pod-wide (spacing, a daily cap, a
//!   shared backoff after a site blocks the pod).
//! - [`search`]: web search behind the `web_search` jack's providers.
//! - [`tasks`]: the prompts for model work on web content (reading a page,
//!   picking from search results, research), run by the agent module
//!   (`construct-agent`), and the checks their answers go through.
//! - [`tools`]: the pod's own `web_search` and `web_fetch` for research.

pub mod events;
pub mod fetch;
pub mod hosts;
pub mod html;
pub mod items;
pub mod path;
pub mod search;
pub mod tasks;
pub mod tools;
pub mod when;
