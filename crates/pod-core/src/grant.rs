//! Grants: a narrower scope on top of an MCP credential's access level
//! (decision `mcp-grants`, extending `mcp-access-levels`).
//!
//! A level says how far a connection may reach (read, edit, outward); a grant
//! says exactly what it may touch, for a credential that runs unattended (the
//! Anticipate jobs, docs/anticipate.md):
//!
//! - `tools`: the only tools it may call (an allowlist; absent: any the level
//!   allows);
//! - `rows`: the only data sets whose rows it may write (a source's names,
//!   e.g. a collection's), checked by the source against every row a call's
//!   transaction writes, not only the one the call names;
//! - `deletes`: the only data sets whose rows it may delete (absent:
//!   wherever it may write rows);
//! - `protected_names`: layouts and components its edit groups may not change,
//!   nor a component one of them embeds (`crate::construct::edit_groups`).
//!
//! The host stores a grant with the credential and sends it beside the level
//! as [`GRANT_HEADER`] (base64url of its JSON), which it strips from every
//! incoming request; a pod trusts it only from its host, with the `mcp`
//! credential. The pod keeps it in the call's task-local beside the level, so
//! the requests the call's tools make through the pod's router are held to it
//! too, and sources read it before a write moves to a blocking thread
//! ([`current`]) and hold the blocking work to it ([`enter`], [`active`]).

use std::cell::RefCell;
use std::collections::BTreeSet;

use axum::http::HeaderMap;
use base64::Engine;
use serde_json::{Value, json};

/// Request header a pod host sets beside [`crate::mcp::ACCESS_HEADER`]: the
/// credential's grant, base64url (no padding) of its JSON. Removed from every
/// incoming request by the host.
pub const GRANT_HEADER: &str = "x-construct-mcp-grant";

/// The most bytes a grant's JSON may take.
pub const MAX_GRANT_BYTES: usize = 8 * 1024;

/// One credential's grant. Every list is a set of exact names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grant {
    /// The tools it may call; `None`: any its level allows.
    pub tools: Option<BTreeSet<String>>,
    /// The collections whose rows it may write; `None`: any.
    pub rows: Option<BTreeSet<String>>,
    /// The collections whose rows it may delete; `None`: those it may write.
    pub deletes: Option<BTreeSet<String>>,
    /// Layouts and components its edit groups may not change.
    pub protected_names: BTreeSet<String>,
}

fn name_set(v: &Value, key: &str) -> Result<Option<BTreeSet<String>>, String> {
    match &v[key] {
        Value::Null => Ok(None),
        Value::Array(items) => {
            let mut out = BTreeSet::new();
            for it in items {
                let s = it
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && s.len() <= 128)
                    .ok_or_else(|| {
                        format!("\"{key}\" lists names: strings of 1 to 128 characters")
                    })?;
                out.insert(s.to_owned());
            }
            Ok(Some(out))
        }
        _ => Err(format!("\"{key}\" is a list of names")),
    }
}

impl Grant {
    /// A grant from its JSON: `{tools?, rows?,
    /// deletes?, no_delete?, protected_names?}`. `no_delete: true`
    /// with no `deletes` allows no deletes.
    pub fn from_json(v: &Value) -> Result<Grant, String> {
        let Some(obj) = v.as_object() else {
            return Err("a grant is a JSON object".into());
        };
        for k in obj.keys() {
            if !matches!(
                k.as_str(),
                "tools" | "rows" | "deletes" | "no_delete" | "protected_names"
            ) {
                return Err(format!(
                    "unknown key \"{k}\": a grant has tools, rows, deletes, no_delete and protected_names"
                ));
            }
        }
        let mut deletes = name_set(v, "deletes")?;
        match &v["no_delete"] {
            Value::Null | Value::Bool(false) => {}
            Value::Bool(true) => {
                if deletes.is_none() {
                    deletes = Some(BTreeSet::new());
                }
            }
            _ => return Err("\"no_delete\" is true or false".into()),
        }
        Ok(Grant {
            tools: name_set(v, "tools")?,
            rows: name_set(v, "rows")?,
            deletes,
            protected_names: name_set(v, "protected_names")?.unwrap_or_default(),
        })
    }

    /// A grant from JSON text (a `--grant` file).
    pub fn from_text(text: &str) -> Result<Grant, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
        Grant::from_json(&v)
    }

    /// Its JSON, as [`Self::from_json`] reads it.
    pub fn to_json(&self) -> Value {
        let list = |s: &BTreeSet<String>| json!(s.iter().collect::<Vec<_>>());
        let mut out = serde_json::Map::new();
        if let Some(t) = &self.tools {
            out.insert("tools".into(), list(t));
        }
        if let Some(r) = &self.rows {
            out.insert("rows".into(), list(r));
        }
        if let Some(d) = &self.deletes {
            out.insert("deletes".into(), list(d));
        }
        if !self.protected_names.is_empty() {
            out.insert("protected_names".into(), list(&self.protected_names));
        }
        Value::Object(out)
    }

    /// Its [`GRANT_HEADER`] value.
    pub fn header_value(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.to_json().to_string())
    }

    /// A grant from a [`GRANT_HEADER`] value; `None` when it doesn't parse
    /// (the caller then refuses everything, never allows more).
    pub fn from_header(v: &str) -> Option<Grant> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(v.trim())
            .ok()?;
        if bytes.len() > MAX_GRANT_BYTES {
            return None;
        }
        Grant::from_json(&serde_json::from_slice(&bytes).ok()?).ok()
    }

    /// A grant that allows nothing: what an unreadable header means.
    pub fn nothing() -> Grant {
        Grant {
            tools: Some(BTreeSet::new()),
            rows: Some(BTreeSet::new()),
            deletes: Some(BTreeSet::new()),
            protected_names: BTreeSet::new(),
        }
    }

    /// Whether it lets the connection call `tool`.
    pub fn allows_tool(&self, tool: &str) -> bool {
        self.tools.as_ref().is_none_or(|t| t.contains(tool))
    }

    /// Whether it lets the connection write rows of `collection`.
    pub fn allows_rows(&self, collection: &str) -> bool {
        self.rows.as_ref().is_none_or(|c| c.contains(collection))
    }

    /// Whether it lets the connection delete rows of `collection`.
    pub fn allows_delete(&self, collection: &str) -> bool {
        self.allows_rows(collection) && self.deletes.as_ref().is_none_or(|c| c.contains(collection))
    }

    /// Whether `name` (a layout or component) is protected.
    pub fn protects(&self, name: &str) -> bool {
        self.protected_names.contains(name)
    }

    /// One line for Settings' Connected apps and `server pods`.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(t) = &self.tools {
            parts.push(format!("{} tools", t.len()));
        }
        if let Some(r) = &self.rows {
            if r.is_empty() {
                parts.push("writes no rows".into());
            } else {
                parts.push(format!(
                    "rows of {}",
                    r.iter().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
        }
        if let Some(d) = &self.deletes {
            if d.is_empty() {
                parts.push("deletes nothing".into());
            } else {
                parts.push(format!(
                    "deletes in {}",
                    d.iter().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
        }
        if !self.protected_names.is_empty() {
            parts.push(format!("{} protected screens", self.protected_names.len()));
        }
        if parts.is_empty() {
            "no limits".into()
        } else {
            parts.join(" · ")
        }
    }
}

/// The grant of the endpoint's caller: an MCP credential's, as its host says
/// ([`GRANT_HEADER`]; an unreadable one allows nothing); `None`: no grant.
pub fn connection_grant(pod: &crate::Pod, headers: &HeaderMap) -> Option<Grant> {
    if pod.host_credential(headers).as_deref() != Some("mcp") {
        return None;
    }
    let v = headers.get(GRANT_HEADER)?;
    Some(
        v.to_str()
            .ok()
            .and_then(Grant::from_header)
            .unwrap_or_else(Grant::nothing),
    )
}

tokio::task_local! {
    /// The grant of the MCP call this task runs (`crate::mcp::call_tool`).
    pub(crate) static CALL_GRANT: Option<Grant>;
}

/// The grant of the MCP call the current task runs for, `None` outside one
/// or for a connection without a grant. Read it before moving work to a
/// blocking thread, and [`enter`] it there.
pub fn current() -> Option<Grant> {
    CALL_GRANT.try_with(|g| g.clone()).ok().flatten()
}

thread_local! {
    static BLOCKING_GRANT: RefCell<Option<Grant>> = const { RefCell::new(None) };
}

/// Holds the grant on this thread until dropped ([`enter`]).
pub struct Entered(Option<Grant>);

impl Drop for Entered {
    fn drop(&mut self) {
        let prev = self.0.take();
        BLOCKING_GRANT.with(|g| *g.borrow_mut() = prev);
    }
}

/// Hold `grant` on this (blocking) thread while the guard lives, so the
/// store code it runs can check it ([`active`]).
pub fn enter(grant: Option<Grant>) -> Entered {
    let prev = BLOCKING_GRANT.with(|g| std::mem::replace(&mut *g.borrow_mut(), grant));
    Entered(prev)
}

/// The grant held on this thread ([`enter`]), else the current task's.
pub fn active() -> Option<Grant> {
    BLOCKING_GRANT.with(|g| g.borrow().clone()).or_else(current)
}

/// The refusal of a call its grant doesn't allow.
pub fn refusal(what: &str) -> Value {
    json!({
        "error": "grant_refuses",
        "detail": format!(
            "This connection's grant doesn't allow {what}. It is a scoped credential for an \
             unattended job; don't try to get around it. Tell the user if the job needs more."
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_checks() {
        let g = Grant::from_json(&json!({
            "tools": ["add_row", "export_collection"],
            "rows": ["needs"],
            "no_delete": true,
            "protected_names": ["home"],
        }))
        .unwrap();
        assert!(g.allows_tool("add_row"));
        assert!(!g.allows_tool("update_layout"));
        assert!(g.allows_rows("needs"));
        assert!(!g.allows_rows("groceries"));
        assert!(!g.allows_delete("needs"));
        assert!(g.protects("home"));
        let back = Grant::from_header(&g.header_value()).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn deletes_follow_rows_unless_listed() {
        let g = Grant::from_json(&json!({
            "rows": ["a", "b"],
            "deletes": ["a"],
        }))
        .unwrap();
        assert!(g.allows_delete("a"));
        assert!(!g.allows_delete("b"));
        let open = Grant::from_json(&json!({ "rows": ["a"] })).unwrap();
        assert!(open.allows_delete("a"));
        assert!(!open.allows_delete("c"));
    }

    #[test]
    fn refuses_unknown_keys_and_bad_headers() {
        assert!(Grant::from_json(&json!({ "tool": ["x"] })).is_err());
        assert!(Grant::from_json(&json!({ "tools": "x" })).is_err());
        assert!(Grant::from_header("not base64 !").is_none());
        let n = Grant::nothing();
        assert!(!n.allows_tool("list_layouts"));
        assert!(!n.allows_rows("x"));
    }

    #[test]
    fn a_blocking_thread_holds_what_it_entered() {
        assert!(active().is_none());
        {
            let _g = enter(Some(Grant::nothing()));
            assert!(active().is_some());
        }
        assert!(active().is_none());
    }
}
