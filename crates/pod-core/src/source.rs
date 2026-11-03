//! Data sources (decision `custom-apps-and-data-sources`): what an adapter
//! offers layouts, declared in its [`SourceManifest`].
//!
//! A data source is an adapter seen from a layout: named feeds that fill an
//! app's root (`GET /v1/workspaces/:id`), screen data paths, the actions its
//! buttons may call, and the entities it serves. An app is a layout plus a
//! list of feeds ([`crate::AppInfo::feeds`]); the core runs each feed and
//! merges the results ([`crate::AppAdapter::feed`]). A source's id is its
//! adapter's [`crate::AppAdapter::name`].
//!
//! Patterns name their variable parts `{name}` (`[a-z_]+`), always a whole
//! path segment, query value or query-key segment:
//! - paths: `/v1/pixelfed/screens/post?id={id}`;
//! - query keys: `post:{id}` (segments split on `:`).
//!
//! A layout matches a pattern when its literal parts are equal and each
//! `{name}` part is non-empty (in a layout, usually a binding such as
//! `$route.id`).

/// Everything a layout may use from one data source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceManifest {
    /// Shown to people (and to Claude) for this source, e.g. `Pixelfed`.
    pub title: &'static str,
    /// The sign-in its data needs (one of the pod's
    /// [`crate::SignInProvider`]s); `None`: it needs none. A feed of a source
    /// whose sign-in the caller lacks is skipped.
    pub sign_in: Option<&'static str>,
    /// The URI schemes of its entities, e.g. `["pixelfed"]`. Unique across
    /// sources; `core` is reserved.
    pub entity_schemes: &'static [&'static str],
    /// Its feeds: what an app's root can show.
    pub feeds: &'static [FeedSpec],
    /// `GET` paths that answer a `DataBundle`: screen data and
    /// `action.data.load`.
    pub data_paths: &'static [PathSpec],
    /// `POST` endpoints a layout's mutations and creates may call.
    pub actions: &'static [PathSpec],
    /// The entities it serves, with their fields.
    pub entities: &'static [EntityDoc],
    /// A dev source (fixture data): custom apps may use it only where dev
    /// apps are shown (`CONSTRUCT_SHOW_DEV_APPS=1`).
    pub dev: bool,
}

impl SourceManifest {
    /// A manifest that offers nothing yet (fill it with struct update
    /// syntax: `SourceManifest { feeds: ..., ..SourceManifest::new("T") }`).
    pub const fn new(title: &'static str) -> Self {
        Self {
            title,
            sign_in: None,
            entity_schemes: &[],
            feeds: &[],
            data_paths: &[],
            actions: &[],
            entities: &[],
            dev: false,
        }
    }

    /// Feed `name`: a feed of that name, else the source's pattern feed
    /// (`{name}`) when `name` is a valid id ([`FeedSpec::name`]).
    pub fn feed(&self, name: &str) -> Option<&FeedSpec> {
        self.feeds
            .iter()
            .find(|f| f.name == name && !f.is_pattern())
            .or_else(|| {
                is_valid_id(name)
                    .then(|| self.feeds.iter().find(|f| f.is_pattern()))
                    .flatten()
            })
    }

    /// Every query-key pattern its feeds and data paths fill.
    pub fn query_keys(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.feeds
            .iter()
            .flat_map(|f| f.query_keys)
            .chain(self.data_paths.iter().flat_map(|p| p.query_keys))
            .copied()
    }
}

/// A named feed ([`crate::AppAdapter::feed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedSpec {
    /// `[a-z0-9_]+`, unique in its source, e.g. `home`. Apps name it, so
    /// never rename one that ships. Or one whole placeholder, `{name}`: a
    /// pattern feed, which an app lists by any `[a-z0-9_]+` name (the
    /// Collections source: one feed per collection, `["collections",
    /// "todos"]`). At most one per source; its query keys may use the
    /// placeholder (`collection:{collection}`), which the feed's name fills.
    pub name: &'static str,
    /// The query keys it fills, e.g. `["timeline:home"]`.
    pub query_keys: &'static [&'static str],
    /// One line for people (and Claude).
    pub doc: &'static str,
}

impl FeedSpec {
    /// Whether this is a pattern feed (`{name}`).
    pub fn is_pattern(&self) -> bool {
        self.name.starts_with('{')
    }
}

/// A data path or action a layout may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathSpec {
    /// A pod path pattern, e.g. `/v1/pixelfed/screens/post?id={id}`.
    pub pattern: &'static str,
    /// The query keys its answer fills (data paths), e.g. `["post:{id}"]`.
    pub query_keys: &'static [&'static str],
    pub doc: &'static str,
}

/// An entity a source serves, for whoever writes layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityDoc {
    /// Its URI pattern, e.g. `pixelfed://posts/{id}`.
    pub uri: &'static str,
    pub doc: &'static str,
    /// `(field, one-line doc)`.
    pub fields: &'static [(&'static str, &'static str)],
}

// ---------- patterns ----------

/// One part of a pattern: literal text or a `{name}` placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part<'a> {
    Lit(&'a str),
    Var,
}

fn is_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// A pattern part; `Err` for text that mixes a placeholder with literal text.
fn pattern_part(s: &str) -> Result<Part<'_>, String> {
    if let Some(name) = s.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        if is_name(name) {
            return Ok(Part::Var);
        }
        return Err(format!("placeholder '{{{name}}}' must be named [a-z_]+"));
    }
    if s.is_empty() || s.contains(['{', '}', '$']) {
        return Err(format!(
            "'{s}' must be literal text or a whole {{name}} placeholder"
        ));
    }
    Ok(Part::Lit(s))
}

/// A parsed path pattern: segments and `(key, value)` query pairs.
struct PathPattern<'a> {
    segments: Vec<Part<'a>>,
    query: Vec<(&'a str, Part<'a>)>,
}

fn parse_path_pattern(p: &str) -> Result<PathPattern<'_>, String> {
    let fail = |e: String| format!("path pattern '{p}': {e}");
    let (path, query) = match p.split_once('?') {
        Some((path, q)) => (path, Some(q)),
        None => (p, None),
    };
    let Some(rest) = path.strip_prefix("/v1/") else {
        return Err(fail("must start with /v1/".into()));
    };
    let segments = rest
        .split('/')
        .map(pattern_part)
        .collect::<Result<Vec<_>, _>>()
        .map_err(fail)?;
    let mut pairs = Vec::new();
    for pair in query.into_iter().flat_map(|q| q.split('&')) {
        let Some((k, v)) = pair.split_once('=') else {
            return Err(fail(format!("query part '{pair}' must be key=value")));
        };
        let Part::Lit(k) = pattern_part(k).map_err(fail)? else {
            return Err(fail(format!("query key '{k}' must be literal")));
        };
        if pairs.iter().any(|(x, _)| *x == k) {
            return Err(fail(format!("query key '{k}' is used twice")));
        }
        pairs.push((k, pattern_part(v).map_err(fail)?));
    }
    Ok(PathPattern {
        segments,
        query: pairs,
    })
}

/// Check that `p` is a valid path pattern.
pub fn check_path_pattern(p: &str) -> Result<(), String> {
    parse_path_pattern(p).map(|_| ())
}

fn part_matches(pattern: Part<'_>, text: &str) -> bool {
    match pattern {
        Part::Lit(l) => l == text,
        Part::Var => !text.is_empty(),
    }
}

/// Whether the layout path `path` (bindings and all) fits `pattern`, a
/// valid path pattern: same segments and the same query keys, in any order.
pub fn path_matches(pattern: &str, path: &str) -> bool {
    let Ok(p) = parse_path_pattern(pattern) else {
        return false;
    };
    let (path, query) = match path.split_once('?') {
        Some((path, q)) => (path, Some(q)),
        None => (path, None),
    };
    let Some(rest) = path.strip_prefix("/v1/") else {
        return false;
    };
    let segments: Vec<&str> = rest.split('/').collect();
    if segments.len() != p.segments.len()
        || !p
            .segments
            .iter()
            .zip(&segments)
            .all(|(pp, s)| part_matches(*pp, s))
    {
        return false;
    }
    let mut pairs = Vec::new();
    for pair in query.into_iter().flat_map(|q| q.split('&')) {
        let Some((k, v)) = pair.split_once('=') else {
            return false;
        };
        if pairs.iter().any(|(x, _)| *x == k) {
            return false;
        }
        pairs.push((k, v));
    }
    pairs.len() == p.query.len()
        && pairs.iter().all(|(k, v)| {
            p.query
                .iter()
                .any(|(pk, pv)| pk == k && part_matches(*pv, v))
        })
}

fn parse_key_pattern(p: &str) -> Result<Vec<Part<'_>>, String> {
    p.split(':')
        .map(pattern_part)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("query-key pattern '{p}': {e}"))
}

/// Check that `p` is a valid query-key pattern.
pub fn check_key_pattern(p: &str) -> Result<(), String> {
    parse_key_pattern(p).map(|_| ())
}

/// Whether the layout query key `key` (bindings and all, e.g.
/// `comments:$route.id`) fits the valid pattern `pattern`.
pub fn key_matches(pattern: &str, key: &str) -> bool {
    let Ok(p) = parse_key_pattern(pattern) else {
        return false;
    };
    let parts: Vec<&str> = key.split(':').collect();
    parts.len() == p.len() && p.iter().zip(&parts).all(|(pp, k)| part_matches(*pp, k))
}

/// Whether some query key matches both patterns (both valid).
pub fn keys_overlap(a: &str, b: &str) -> bool {
    let (Ok(a), Ok(b)) = (parse_key_pattern(a), parse_key_pattern(b)) else {
        return false;
    };
    a.len() == b.len()
        && a.iter().zip(&b).all(|pair| match pair {
            (Part::Lit(x), Part::Lit(y)) => x == y,
            _ => true,
        })
}

/// Whether `id` is a valid feed name or entity scheme (`[a-z0-9_]+`).
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

// ---------- checks ----------

/// Check one manifest on its own: valid names and patterns, unique feeds.
pub fn check_manifest(source: &str, m: &SourceManifest) -> Result<(), String> {
    let fail = |e: String| Err(format!("source '{source}': {e}"));
    for s in m.entity_schemes {
        if !is_valid_id(s) {
            return fail(format!("entity scheme '{s}' is not [a-z0-9_]+"));
        }
        if *s == "core" {
            return fail("the entity scheme 'core' is reserved".into());
        }
    }
    for (i, f) in m.feeds.iter().enumerate() {
        let pattern = f.is_pattern() && matches!(pattern_part(f.name), Ok(Part::Var));
        if !is_valid_id(f.name) && !pattern {
            return fail(format!(
                "feed name '{}' is not [a-z0-9_]+ or one {{name}} placeholder",
                f.name
            ));
        }
        if pattern && m.feeds[..i].iter().any(FeedSpec::is_pattern) {
            return fail("a source has at most one pattern feed".into());
        }
        if m.feeds[..i].iter().any(|o| o.name == f.name) {
            return fail(format!("feed '{}' is declared twice", f.name));
        }
    }
    for p in m.data_paths.iter().chain(m.actions) {
        if let Err(e) = check_path_pattern(p.pattern) {
            return fail(e);
        }
    }
    for k in m.query_keys() {
        if let Err(e) = check_key_pattern(k) {
            return fail(e);
        }
    }
    Ok(())
}

/// Every pod path a layout uses: `(pointer-ish place, path, is_action)`.
/// `is_action`: a mutation's or create's endpoint (else a data path).
fn layout_routes(node: &protocol::UiNode) -> Vec<(String, &str, bool)> {
    use protocol::{ActionIntent, UiNode};
    let mut out = Vec::new();
    if let UiNode::Workspace { screens, .. } = node {
        for s in screens {
            if let Some(data) = &s.data {
                out.push((format!("screen '{}' data", s.id), data.as_str(), false));
            }
        }
    }
    for intent in crate::validate::intents(node) {
        match intent {
            ActionIntent::OptimisticMutate { endpoint, .. } => {
                out.push(("a mutation's endpoint".into(), endpoint.as_str(), true));
            }
            ActionIntent::Create { endpoint, .. } => {
                out.push(("a create's endpoint".into(), endpoint.as_str(), true));
            }
            ActionIntent::LoadData { path } => {
                out.push(("an action.data.load path".into(), path.as_str(), false));
            }
            _ => {}
        }
    }
    out
}

/// The capability check of a layout (decision `custom-apps-and-data-sources`):
/// every screen data path and `action.data.load` path fits a data path of
/// one of `sources`, and every mutation and create endpoint fits one of
/// their actions. `sources`: `(source id, manifest)`. `Err` names the path
/// and what the sources offer instead.
pub fn check_routes(
    node: &protocol::UiNode,
    sources: &[(&str, &SourceManifest)],
) -> Result<(), String> {
    for (place, path, is_action) in layout_routes(node) {
        let offered = || {
            sources.iter().flat_map(move |(_, m)| {
                if is_action { m.actions } else { m.data_paths }
                    .iter()
                    .map(|p| p.pattern)
            })
        };
        if offered().any(|p| path_matches(p, path)) {
            continue;
        }
        let names: Vec<&str> = sources.iter().map(|(id, _)| *id).collect();
        let list: Vec<&str> = offered().collect();
        return Err(format!(
            "{place} '{path}' is not {} of its data sources ({}); they offer: {}",
            if is_action {
                "an action"
            } else {
                "a data path"
            },
            if names.is_empty() {
                "none".to_owned()
            } else {
                names.join(", ")
            },
            if list.is_empty() {
                "nothing".to_owned()
            } else {
                list.join(", ")
            }
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_patterns_match_bound_layout_paths() {
        let p = "/v1/x/screens/post?id={id}";
        assert!(path_matches(p, "/v1/x/screens/post?id=$route.id"));
        assert!(path_matches(p, "/v1/x/screens/post?id=42"));
        assert!(!path_matches(p, "/v1/x/screens/post?id="));
        assert!(!path_matches(p, "/v1/x/screens/post"));
        assert!(!path_matches(p, "/v1/x/screens/post?id=1&more=2"));
        assert!(!path_matches(p, "/v1/x/screens/post?post_id=1"));
        assert!(!path_matches(p, "/v1/x/screens/posts?id=1"));
        assert!(!path_matches(p, "/v1/x/screens/post/a?id=1"));
        assert!(!path_matches(p, "/v1/x/screens/post?id=1&id=2"));
        let q = "/v1/x/a?k=lit&id={id}";
        assert!(path_matches(q, "/v1/x/a?id=3&k=lit"));
        assert!(!path_matches(q, "/v1/x/a?id=3&k=other"));
        let seg = "/v1/x/{id}/like";
        assert!(path_matches(seg, "/v1/x/$item.id/like"));
        assert!(!path_matches(seg, "/v1/x//like"));
        assert!(path_matches("/v1/x/like", "/v1/x/like"));
        assert!(!path_matches("/v1/x/like", "/v1/y/like"));
        assert!(!path_matches("/v1/x/like", "/v2/x/like"));
    }

    #[test]
    fn bad_patterns_are_refused() {
        for bad in [
            "/v2/x",
            "/v1/x/{ID}",
            "/v1/x/a{id}",
            "/v1/x//y",
            "/v1/x?id",
            "/v1/x?{k}=1",
            "/v1/x?id={a}&id={b}",
            "/v1/x/$route.id",
        ] {
            assert!(check_path_pattern(bad).is_err(), "{bad}");
        }
        for bad in ["post:", "post:{Id}", "a:b{c}", ""] {
            assert!(check_key_pattern(bad).is_err(), "{bad}");
        }
        assert_eq!(check_key_pattern("thing:{id}:steps"), Ok(()));
    }

    #[test]
    fn a_pattern_feed_answers_to_any_name() {
        let m = SourceManifest {
            feeds: &[
                FeedSpec {
                    name: "all",
                    query_keys: &["c:all"],
                    doc: "",
                },
                FeedSpec {
                    name: "{collection}",
                    query_keys: &["collection:{collection}"],
                    doc: "",
                },
            ],
            ..SourceManifest::new("C")
        };
        assert_eq!(check_manifest("c", &m), Ok(()));
        assert_eq!(m.feed("all").map(|f| f.name), Some("all"));
        assert_eq!(m.feed("todos").map(|f| f.name), Some("{collection}"));
        assert!(m.feed("Todos!").is_none());
        assert!(m.feed("{collection}").is_none());
        let two = SourceManifest {
            feeds: &[
                FeedSpec {
                    name: "{a}",
                    query_keys: &[],
                    doc: "",
                },
                FeedSpec {
                    name: "{b}",
                    query_keys: &[],
                    doc: "",
                },
            ],
            ..SourceManifest::new("C")
        };
        assert!(check_manifest("c", &two).is_err());
    }

    #[test]
    fn overlapping_query_keys_are_found() {
        assert!(keys_overlap("post:{id}", "post:{x}"));
        assert!(keys_overlap("post:{id}", "post:7"));
        assert!(keys_overlap("{a}:all", "things:{b}"));
        assert!(!keys_overlap("post:{id}", "posts:{id}"));
        assert!(!keys_overlap("things:all", "thing:{id}:steps"));
        assert!(!keys_overlap("timeline:home", "timeline:local"));
    }
}
