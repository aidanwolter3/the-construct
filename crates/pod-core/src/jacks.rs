//! Jacks (decision `jacks`): the extra abilities a pod is plugged into.
//!
//! A jack is one thing the pod can do beyond holding layouts and data: ask a
//! model, fetch websites, search the web, follow an agent runner, show a
//! Pixelfed account. Each pod has the same set of jacks, declared by the core
//! ([`core_jacks`]) and by adapters ([`crate::AppAdapter::jacks`]); what
//! differs per pod is which are plugged and how they are configured.
//!
//! Three kinds:
//!
//! - **Switch** ([`JackKind::Switch`]): on or off, using the server's own
//!   resources (its model key, its network). `model` and `web`.
//! - **Config** ([`JackKind::Config`]): plugged by giving it values, some of
//!   them secret (sealed with the server key, never shown back). `web_search`:
//!   a search provider and its key. The key is the pod's own, not the
//!   server's.
//! - **Account** ([`JackKind::Account`]): an account the adapter connects
//!   through its own flow (a sign-in form or OAuth). The adapter reports its
//!   state ([`crate::AppAdapter::jack_status`]) and may unplug it
//!   ([`crate::AppAdapter::unplug_jack`]). `pixelfed`, `agent_runner`.
//!
//! The host can **lock** a jack off for a pod (`jack.<id> = locked` in
//! `pod_settings`, written when the pod is made): temporary pods, which
//! anyone can make, have `model` and `web` locked so a visitor can't spend
//! the server's model key or make it fetch URLs of their choosing. A locked
//! jack reads as unplugged and can't be plugged.
//!
//! State lives in `jacks` (one row per jack that was ever plugged or
//! unplugged; no row means the jack's default).

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};

use crate::adapter::Pod;
use crate::store::Store;

/// The model jack: model features (computed fields, reading pages, agent
/// fills) with the server's model key.
pub const MODEL: &str = "model";
/// The web jack: fetching websites (fillers, previews).
pub const WEB: &str = "web";
/// The web search jack: searching the web with the pod's own provider key.
pub const WEB_SEARCH: &str = "web_search";

/// The `pod_settings` value that locks a jack off (`jack.<id>`).
pub const LOCKED: &str = "locked";

/// One value a [`JackKind::Config`] jack takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JackField {
    pub name: &'static str,
    pub doc: &'static str,
    /// Sealed at rest and never shown back (an API key).
    pub secret: bool,
    pub required: bool,
    /// The values it may take (empty: any text).
    pub options: &'static [&'static str],
}

/// What kind of jack it is; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JackKind {
    Switch {
        /// Plugged on a pod that never chose.
        default_on: bool,
    },
    Config {
        fields: &'static [JackField],
    },
    Account {
        /// How to plug it (where its sign-in is), for Claude and people.
        how: &'static str,
    },
}

impl JackKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Switch { .. } => "switch",
            Self::Config { .. } => "config",
            Self::Account { .. } => "account",
        }
    }
}

/// A jack: declared once, the same on every pod.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JackSpec {
    /// Unique across the core and every adapter; stored, so never renamed.
    pub id: &'static str,
    pub title: &'static str,
    /// What plugging it lets the pod do.
    pub summary: &'static str,
    pub kind: JackKind,
}

/// An account jack's state, from its adapter.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JackStatus {
    pub plugged: bool,
    /// e.g. "@ana · pixelfed.example", "https://runner.example".
    pub detail: Option<String>,
}

/// The web search providers the `web_search` jack knows.
pub const SEARCH_PROVIDERS: &[&str] = &["brave", "searxng", "duckduckgo"];

const SEARCH_FIELDS: &[JackField] = &[
    JackField {
        name: "provider",
        doc: "brave (Brave Search API: a key from api-dashboard.search.brave.com), searxng (your \
              own SearXNG instance with JSON output: base_url) or duckduckgo (its HTML page, no \
              key; it blocks repeated automated searches, so it is capped at a few a day)",
        secret: false,
        required: true,
        options: SEARCH_PROVIDERS,
    },
    JackField {
        name: "api_key",
        doc: "the provider's API key (brave); sealed and never shown back",
        secret: true,
        required: false,
        options: &[],
    },
    JackField {
        name: "base_url",
        doc: "the instance's https:// address on a public host name (searxng)",
        secret: false,
        required: false,
        options: &[],
    },
];

/// The jacks the core declares (the adapters' come after them).
pub fn core_jacks() -> Vec<JackSpec> {
    vec![
        JackSpec {
            id: MODEL,
            title: "Model",
            summary: "Model features with the server's model key: computed fields that ask a \
                      model, fillers that read pages (extract llm) or search (agent), and \
                      shopping-style picks from search results.",
            kind: JackKind::Switch { default_on: true },
        },
        JackSpec {
            id: WEB,
            title: "Web",
            summary: "Fetching websites: collection fillers (the Events app's sites \
                      included) and fetch previews.",
            kind: JackKind::Switch { default_on: true },
        },
        JackSpec {
            id: WEB_SEARCH,
            title: "Web search",
            summary: "Searching the web with this pod's own search provider: fillers with \
                      query_field, and the search tool of agent fills.",
            kind: JackKind::Config {
                fields: SEARCH_FIELDS,
            },
        },
    ]
}

fn setting_key(id: &str) -> String {
    format!("jack.{id}")
}

fn secret_context(id: &str, field: &str) -> String {
    format!("jacks:{id}:{field}")
}

/// Whether the host locked jack `id` off for this pod.
pub fn locked_on(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    Ok(crate::store::pod_setting_on(conn, &setting_key(id))?.is_some_and(|v| v == LOCKED))
}

/// Lock jack `id` off (the host's choice, e.g. in a migration that turns an
/// older setting into a lock).
pub fn lock_on(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO pod_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![setting_key(id), LOCKED],
    )?;
    Ok(())
}

/// The `pod_settings` rows that lock `ids` off, for a host making a pod.
pub fn lock_settings(ids: &[&str]) -> Vec<(String, String)> {
    ids.iter()
        .map(|id| (setting_key(id), LOCKED.to_owned()))
        .collect()
}

struct Row {
    plugged: bool,
    config: Map<String, Value>,
    secrets: Map<String, Value>,
}

fn row_on(conn: &Connection, id: &str) -> rusqlite::Result<Option<Row>> {
    conn.query_row(
        "SELECT plugged, config, secrets FROM jacks WHERE id = ?1",
        [id],
        |r| {
            let parse = |s: String| match serde_json::from_str::<Value>(&s) {
                Ok(Value::Object(m)) => m,
                _ => Map::new(),
            };
            Ok(Row {
                plugged: r.get::<_, i64>(0)? != 0,
                config: parse(r.get(1)?),
                secrets: parse(r.get(2)?),
            })
        },
    )
    .optional()
}

/// Whether jack `id` (of kind switch or config) is plugged on this pod:
/// not locked, and plugged by choice or by default.
pub fn plugged_on(conn: &Connection, spec: &JackSpec) -> rusqlite::Result<bool> {
    if locked_on(conn, spec.id)? {
        return Ok(false);
    }
    Ok(match row_on(conn, spec.id)? {
        Some(r) => r.plugged,
        None => matches!(spec.kind, JackKind::Switch { default_on: true }),
    })
}

/// [`plugged_on`] by id inside a connection, for a core jack.
pub fn core_plugged_on(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    match core_jacks().into_iter().find(|j| j.id == id) {
        Some(spec) => plugged_on(conn, &spec),
        None => Ok(false),
    }
}

/// [`plugged_on`] by id, for a core jack (`model`, `web`, `web_search`).
/// An unknown id is unplugged.
pub fn core_plugged(store: &Store, id: &str) -> bool {
    let Some(spec) = core_jacks().into_iter().find(|j| j.id == id) else {
        return false;
    };
    store.with_conn(|c| plugged_on(c, &spec)).unwrap_or(false)
}

/// A plugged config jack's values, secrets opened, by field name; `None`
/// when it isn't plugged (or is locked).
pub fn config(store: &Store, id: &str) -> Option<Map<String, Value>> {
    let row = store.with_conn(|c| {
        if locked_on(c, id)? {
            return Ok(None);
        }
        row_on(c, id)
    });
    let row = row.ok().flatten().filter(|r| r.plugged)?;
    let mut out = row.config;
    for (k, v) in row.secrets {
        if let Some(sealed) = v.as_str()
            && let Ok(plain) = store.secrets().open(&secret_context(id, &k), sealed)
        {
            out.insert(k, json!(plain));
        }
    }
    Some(out)
}

/// Why a plug or unplug was refused.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub error: &'static str,
    pub detail: String,
}

impl Refusal {
    fn new(error: &'static str, detail: impl Into<String>) -> Self {
        Self {
            error,
            detail: detail.into(),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({ "error": self.error, "detail": self.detail })
    }
}

/// Check `values` for config jack `fields`: unknown names, options, and
/// required fields (a secret already stored counts as given).
fn check_values(
    id: &str,
    fields: &[JackField],
    values: &Map<String, Value>,
    stored_secrets: &Map<String, Value>,
) -> Result<(), Refusal> {
    for (k, v) in values {
        let Some(f) = fields.iter().find(|f| f.name == k) else {
            let names: Vec<&str> = fields.iter().map(|f| f.name).collect();
            return Err(Refusal::new(
                "invalid_config",
                format!("{id} has no field '{k}' (fields: {})", names.join(", ")),
            ));
        };
        let Some(s) = v.as_str().filter(|s| !s.trim().is_empty()) else {
            if v.is_null() {
                continue;
            }
            return Err(Refusal::new("invalid_config", format!("{k} is text")));
        };
        if s.len() > 2_000 {
            return Err(Refusal::new("invalid_config", format!("{k} is too long")));
        }
        if !f.options.is_empty() && !f.options.contains(&s.trim()) {
            return Err(Refusal::new(
                "invalid_config",
                format!("{k} is one of {}", f.options.join(", ")),
            ));
        }
    }
    for f in fields.iter().filter(|f| f.required) {
        let given = values
            .get(f.name)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty());
        if !given && !(f.secret && stored_secrets.contains_key(f.name)) {
            return Err(Refusal::new(
                "invalid_config",
                format!("{id} needs {}", f.name),
            ));
        }
    }
    if id == WEB_SEARCH {
        check_search(values, stored_secrets)?;
    }
    Ok(())
}

/// The `web_search` jack's own rule: each provider's needs.
fn check_search(values: &Map<String, Value>, stored: &Map<String, Value>) -> Result<(), Refusal> {
    let text = |k: &str| {
        values
            .get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    match text("provider") {
        Some("brave") if text("api_key").is_none() && !stored.contains_key("api_key") => {
            Err(Refusal::new("invalid_config", "brave needs api_key"))
        }
        Some("searxng") => {
            let Some(base) = text("base_url") else {
                return Err(Refusal::new("invalid_config", "searxng needs base_url"));
            };
            // A public host name, as for every other address a pod names:
            // an IP literal or an internal name would let the pod's searches
            // reach the server's own network.
            match reqwest::Url::parse(base) {
                Ok(u)
                    if u.scheme() == "https"
                        && u.host_str().is_some_and(crate::net::is_public_host) =>
                {
                    Ok(())
                }
                _ => Err(Refusal::new(
                    "invalid_config",
                    "base_url is an https:// address on a public host name",
                )),
            }
        }
        _ => Ok(()),
    }
}

/// Plug switch or config jack `spec` with `values` (config only; secrets
/// left out keep their stored value).
pub fn plug(
    store: &Store,
    spec: &JackSpec,
    values: &Map<String, Value>,
    now: i64,
) -> Result<(), Refusal> {
    let fields: &[JackField] = match spec.kind {
        JackKind::Switch { .. } => {
            if !values.is_empty() {
                return Err(Refusal::new(
                    "invalid_config",
                    format!("{} is a switch: it takes no config", spec.id),
                ));
            }
            &[]
        }
        JackKind::Config { fields } => fields,
        JackKind::Account { how } => {
            return Err(Refusal::new("account_jack", how.to_owned()));
        }
    };
    let id = spec.id;
    let secrets = store.secrets().clone();
    let r = store.transaction(|tx| {
        if locked_on(tx, id)? {
            return Ok(Err(Refusal::new(
                "jack_locked",
                format!("this server keeps {id} unplugged on this pod"),
            )));
        }
        let old = row_on(tx, id)?;
        let (mut config, mut sealed) = old.map(|r| (r.config, r.secrets)).unwrap_or_default();
        if let Err(e) = check_values(id, fields, values, &sealed) {
            return Ok(Err(e));
        }
        if !values.is_empty() {
            config = Map::new();
        }
        for f in fields {
            let Some(v) = values.get(f.name) else {
                continue;
            };
            let v = v.as_str().map(str::trim).filter(|s| !s.is_empty());
            match (f.secret, v) {
                (true, Some(s)) => {
                    sealed.insert(
                        f.name.into(),
                        json!(secrets.seal(&secret_context(id, f.name), s)),
                    );
                }
                (true, None) => {
                    sealed.remove(f.name);
                }
                (false, Some(s)) => {
                    config.insert(f.name.into(), json!(s));
                }
                (false, None) => {}
            }
        }
        tx.execute(
            "INSERT INTO jacks (id, plugged, config, secrets, updated_at) VALUES (?1, 1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET plugged = 1, config = excluded.config,
                 secrets = excluded.secrets, updated_at = excluded.updated_at",
            params![
                id,
                Value::Object(config).to_string(),
                Value::Object(sealed).to_string(),
                now
            ],
        )?;
        Ok(Ok(()))
    });
    r.map_err(|e| Refusal::new("store_error", e.to_string()))?
}

/// Unplug switch or config jack `id`. A config jack forgets its values and
/// secrets.
pub fn unplug(store: &Store, spec: &JackSpec, now: i64) -> Result<(), Refusal> {
    if let JackKind::Account { .. } = spec.kind {
        return Err(Refusal::new(
            "account_jack",
            "an account jack is unplugged by its adapter",
        ));
    }
    store
        .with_conn(|c| {
            c.execute(
                "INSERT INTO jacks (id, plugged, config, secrets, updated_at) VALUES (?1, 0, '{}', '{}', ?2)
                 ON CONFLICT (id) DO UPDATE SET plugged = 0, config = '{}', secrets = '{}',
                     updated_at = excluded.updated_at",
                params![spec.id, now],
            )
        })
        .map(|_| ())
        .map_err(|e| Refusal::new("store_error", e.to_string()))
}

/// Every jack of the pod: the core's, then each adapter's, with the index
/// of the adapter that declared it.
pub fn all(pod: &Pod) -> Vec<(JackSpec, Option<usize>)> {
    let mut out: Vec<(JackSpec, Option<usize>)> =
        core_jacks().into_iter().map(|j| (j, None)).collect();
    for (i, a) in pod.catalog().adapters().iter().enumerate() {
        out.extend(a.jacks().into_iter().map(|j| (j, Some(i))));
    }
    out
}

/// Find jack `id`.
pub fn find(pod: &Pod, id: &str) -> Option<(JackSpec, Option<usize>)> {
    all(pod).into_iter().find(|(j, _)| j.id == id)
}

/// Whether jack `id` (any kind) is plugged on this pod.
pub async fn is_plugged(pod: &Pod, id: &str) -> bool {
    let Some((spec, adapter)) = find(pod, id) else {
        return false;
    };
    describe(pod, &spec, adapter).await["state"] == "plugged"
}

/// One jack as `list_jacks` and `GET /v1/jacks` show it.
pub async fn describe(pod: &Pod, spec: &JackSpec, adapter: Option<usize>) -> Value {
    let id = spec.id;
    let (locked, row) = pod
        .store()
        .with_conn(|c| Ok::<_, rusqlite::Error>((locked_on(c, id)?, row_on(c, id)?)))
        .unwrap_or((false, None));
    let mut out = json!({
        "id": id,
        "title": spec.title,
        "summary": spec.summary,
        "kind": spec.kind.as_str(),
    });
    let (state, detail) = match spec.kind {
        _ if locked => (
            "locked",
            Some("this server keeps it unplugged on this pod".to_owned()),
        ),
        JackKind::Account { how } => {
            let st = match adapter.and_then(|i| pod.catalog().adapters().get(i)) {
                Some(a) => a.jack_status(pod, id).await.unwrap_or_default(),
                None => JackStatus::default(),
            };
            out["how"] = json!(how);
            (if st.plugged { "plugged" } else { "unplugged" }, st.detail)
        }
        JackKind::Switch { default_on } => {
            let on = row.as_ref().map_or(default_on, |r| r.plugged);
            (if on { "plugged" } else { "unplugged" }, None)
        }
        JackKind::Config { fields } => {
            let plugged = row.as_ref().is_some_and(|r| r.plugged);
            let values: Vec<Value> = fields
                .iter()
                .map(|f| {
                    let mut v = json!({
                        "name": f.name,
                        "doc": f.doc,
                        "secret": f.secret,
                        "required": f.required,
                    });
                    if !f.options.is_empty() {
                        v["options"] = json!(f.options);
                    }
                    if let Some(r) = row.as_ref().filter(|r| r.plugged) {
                        if f.secret {
                            v["set"] = json!(r.secrets.contains_key(f.name));
                        } else if let Some(x) = r.config.get(f.name) {
                            v["value"] = x.clone();
                        }
                    }
                    v
                })
                .collect();
            out["fields"] = json!(values);
            let detail = row
                .as_ref()
                .filter(|r| r.plugged)
                .and_then(|r| r.config.get("provider"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            (if plugged { "plugged" } else { "unplugged" }, detail)
        }
    };
    out["state"] = json!(state);
    if let Some(d) = detail {
        out["detail"] = json!(d);
    }
    out
}

/// Every jack, described.
pub async fn list(pod: &Pod) -> Vec<Value> {
    let mut out = Vec::new();
    for (spec, adapter) in all(pod) {
        out.push(describe(pod, &spec, adapter).await);
    }
    out
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// MCP `plug_jack {id, config?}`.
pub async fn plug_tool(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let id = args["id"].as_str().unwrap_or("");
    let Some((spec, adapter)) = find(pod, id) else {
        return Err(unknown(pod, id));
    };
    let values = match &args["config"] {
        Value::Null => Map::new(),
        Value::Object(m) => m.clone(),
        _ => {
            return Err(json!({ "error": "invalid_arguments", "detail": "config is an object" }));
        }
    };
    let s2 = spec;
    let r = crate::blocking(pod, move |s| Ok(plug(s, &s2, &values, now_secs()))).await;
    match r {
        Ok(Ok(())) => Ok(json!({ "ok": true, "jack": describe(pod, &spec, adapter).await })),
        Ok(Err(e)) => Err(e.to_json()),
        Err(_) => Err(json!({ "error": "store_error" })),
    }
}

/// MCP `unplug_jack {id}`.
pub async fn unplug_tool(pod: &Pod, args: &Value) -> Result<Value, Value> {
    let id = args["id"].as_str().unwrap_or("");
    let Some((spec, adapter)) = find(pod, id) else {
        return Err(unknown(pod, id));
    };
    if let JackKind::Account { .. } = spec.kind {
        let a = adapter.and_then(|i| pod.catalog().adapters().get(i).cloned());
        let Some(f) = a.as_ref().and_then(|a| a.unplug_jack(pod, id)) else {
            return Err(json!({
                "error": "account_jack",
                "detail": "this account is unplugged where it was signed in (Settings → Sign-ins)",
            }));
        };
        return match f.await {
            Ok(()) => Ok(json!({ "ok": true, "jack": describe(pod, &spec, adapter).await })),
            Err(e) => Err(json!({ "error": "unplug_failed", "detail": e })),
        };
    }
    let s2 = spec;
    match crate::blocking(pod, move |s| Ok(unplug(s, &s2, now_secs()))).await {
        Ok(Ok(())) => Ok(json!({ "ok": true, "jack": describe(pod, &spec, adapter).await })),
        Ok(Err(e)) => Err(e.to_json()),
        Err(_) => Err(json!({ "error": "store_error" })),
    }
}

fn unknown(pod: &Pod, id: &str) -> Value {
    let ids: Vec<&str> = all(pod).iter().map(|(j, _)| j.id).collect();
    json!({
        "error": "unknown_jack",
        "detail": format!("no jack '{id}' (jacks: {})", ids.join(", ")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn search() -> JackSpec {
        core_jacks()
            .into_iter()
            .find(|j| j.id == WEB_SEARCH)
            .unwrap()
    }

    fn model() -> JackSpec {
        core_jacks().into_iter().find(|j| j.id == MODEL).unwrap()
    }

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn switches_default_on_and_unplug() {
        let s = store();
        assert!(core_plugged(&s, MODEL));
        unplug(&s, &model(), 1).unwrap();
        assert!(!core_plugged(&s, MODEL));
        plug(&s, &model(), &Map::new(), 2).unwrap();
        assert!(core_plugged(&s, MODEL));
    }

    #[test]
    fn a_locked_jack_is_unplugged_and_refuses_plugging() {
        let s = store();
        s.with_conn(|c| lock_on(c, MODEL)).unwrap();
        assert!(!core_plugged(&s, MODEL));
        let e = plug(&s, &model(), &Map::new(), 1).unwrap_err();
        assert_eq!(e.error, "jack_locked");
    }

    #[test]
    fn config_jack_keeps_its_secret_and_needs_its_fields() {
        let s = store();
        assert!(!core_plugged(&s, WEB_SEARCH));
        let e = plug(&s, &search(), &obj(json!({"provider": "brave"})), 1).unwrap_err();
        assert!(e.detail.contains("api_key"), "{e:?}");
        plug(
            &s,
            &search(),
            &obj(json!({"provider": "brave", "api_key": "k-1"})),
            1,
        )
        .unwrap();
        assert!(core_plugged(&s, WEB_SEARCH));
        let c = config(&s, WEB_SEARCH).unwrap();
        assert_eq!(c["api_key"], "k-1");
        // Re-plugging without the key keeps it.
        plug(&s, &search(), &obj(json!({"provider": "brave"})), 2).unwrap();
        assert_eq!(config(&s, WEB_SEARCH).unwrap()["api_key"], "k-1");
        let e = plug(&s, &search(), &obj(json!({"provider": "bing"})), 3).unwrap_err();
        assert!(e.detail.contains("one of"), "{e:?}");
        let e = plug(
            &s,
            &search(),
            &obj(json!({"provider": "searxng", "base_url": "http://x.example"})),
            3,
        )
        .unwrap_err();
        assert!(e.detail.contains("https"), "{e:?}");
        for base in [
            "https://10.0.0.5",
            "https://127.0.0.1:8443",
            "https://[::1]",
            "https://[fdaa::3]:8443",
            "https://169.254.169.254",
            "https://searx.internal",
            "https://localhost",
        ] {
            let e = plug(
                &s,
                &search(),
                &obj(json!({"provider": "searxng", "base_url": base})),
                3,
            )
            .unwrap_err();
            assert!(e.detail.contains("public host"), "{base}: {e:?}");
        }
        plug(
            &s,
            &search(),
            &obj(json!({"provider": "searxng", "base_url": "https://searx.example"})),
            3,
        )
        .unwrap();
        unplug(&s, &search(), 4).unwrap();
        assert!(config(&s, WEB_SEARCH).is_none());
    }

    #[test]
    fn lock_settings_name_the_jacks() {
        assert_eq!(
            lock_settings(&[MODEL]),
            vec![("jack.model".to_owned(), "locked".to_owned())]
        );
    }
}
