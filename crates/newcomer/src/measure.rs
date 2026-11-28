//! What the agent built, read back from the pod, and the measures of work
//! it redid (decision `newcomer-journeys`): shipped pieces rebuilt, other
//! pieces the pod's reuse hints match, the time those writes took, refused
//! calls and gaps.

use crate::claude::Call;
use crate::pod::Pod;
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// One of the pod's own layouts.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Layout {
    pub id: String,
    /// How home reaches it; `None`: home doesn't.
    pub reached: Option<String>,
    pub copy_of: Option<String>,
    pub layout: Value,
}

/// What the pod holds at the end of a persona.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Built {
    pub layouts: Vec<Layout>,
    /// The pod's own components (`@me/<name>`) and their elements.
    pub components: Vec<(String, Value)>,
}

impl Built {
    pub fn read(pod: &Pod) -> Result<Built, String> {
        let mut b = Built::default();
        let list = pod.call("list_layouts", json!({}))?;
        for l in list["layouts"].as_array().into_iter().flatten() {
            let id = l["id"].as_str().unwrap_or_default().to_owned();
            let got = pod.call("get_layout", json!({"id": id}))?;
            b.layouts.push(Layout {
                reached: l["reached"].as_str().map(str::to_owned),
                copy_of: l["copy_of"].as_str().map(str::to_owned),
                layout: got["layout"].clone(),
                id,
            });
        }
        let comps = pod.call("list_components", json!({}))?;
        for c in comps["components"].as_array().into_iter().flatten() {
            if c["scope"] != "own" {
                continue;
            }
            let name = c["name"].as_str().unwrap_or_default().to_owned();
            let got = pod.call("get_component", json!({"name": name}))?;
            b.components.push((name, got));
        }
        Ok(b)
    }

    /// Every element the pod's own layouts and components reference
    /// (`core://Use`, `core://Layout`, whole references), as `@scope/name`.
    pub fn references(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for l in &self.layouts {
            refs(&l.layout, &mut out);
        }
        for (_, c) in &self.components {
            refs(c, &mut out);
        }
        out
    }

    /// Own layouts home doesn't reach.
    pub fn unreached(&self) -> Vec<String> {
        self.layouts
            .iter()
            .filter(|l| l.reached.is_none())
            .map(|l| l.id.clone())
            .collect()
    }
}

fn refs(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if k == "ref" {
                    let name = x.as_str().or_else(|| x["name"].as_str());
                    if let Some(n) = name.filter(|n| n.starts_with('@')) {
                        out.insert(element_name(n));
                    }
                }
                refs(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| refs(x, out)),
        _ => {}
    }
}

/// `@core/agents/slot:agents` (a part) is `@core/agents`.
fn element_name(n: &str) -> String {
    n.splitn(3, '/').take(2).collect::<Vec<_>>().join("/")
}

/// A shipped piece the agent didn't use.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Rebuilt {
    pub name: String,
    /// `copied` (an own copy of it) or `not used`.
    pub how: String,
}

/// The persona's `uses:` pieces that the pod doesn't reference: copied or
/// not used at all.
pub fn shipped_rebuilt(uses: &[String], built: &Built) -> Vec<Rebuilt> {
    let used = built.references();
    uses.iter()
        .filter(|u| !used.contains(*u))
        .map(|u| Rebuilt {
            name: u.clone(),
            how: if built
                .layouts
                .iter()
                .any(|l| l.copy_of.as_deref() == Some(u.as_str()))
            {
                "copied".into()
            } else {
                "not used".into()
            },
        })
        .collect()
}

/// Whether a write's warning is a reuse hint: a piece that a component
/// already draws, or one written several times over (`pod_core::hints`).
pub fn is_reuse_hint(w: &str) -> bool {
    (w.contains(": this is what ") && w.contains(" draws: use "))
        || (w.contains("is written ") && w.contains(" times here"))
}

/// The reuse hints the pod gives for the final layouts (`validate_layout`
/// answers the same advice a write does), and for the pod's copies of core
/// layouts other than home's start.
pub fn hints_left(pod: &Pod, built: &Built) -> Vec<String> {
    let mut out = Vec::new();
    for l in &built.layouts {
        if let Ok(v) = pod.call("validate_layout", json!({"layout": l.layout})) {
            for w in v["warnings"].as_array().into_iter().flatten() {
                if let Some(w) = w.as_str().filter(|w| is_reuse_hint(w)) {
                    out.push(format!("{}: {w}", l.id));
                }
            }
        }
        if let Some(c) = &l.copy_of
            && !(l.id == "home" && c == "@core/shell")
        {
            out.push(format!("{}: a copy of {c}", l.id));
        }
    }
    out
}

const WRITES: [&str; 4] = [
    "create_layout",
    "update_layout",
    "define_component",
    "update_component",
];

/// Measures read from the agent's calls.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Calls {
    pub tool_calls: usize,
    pub refused: Vec<String>,
    pub gaps: Vec<String>,
    /// Writes whose answer carried a reuse hint, and the time they took.
    pub rebuilding_writes: usize,
    pub rebuilding_ms: u64,
}

pub fn calls(all: &[Call]) -> Calls {
    let mut m = Calls {
        tool_calls: all.len(),
        ..Calls::default()
    };
    for c in all {
        if c.is_error {
            m.refused
                .push(format!("{}: {}", c.name, refusal(&c.output)));
        }
        if c.name == "note_gap"
            && let Some(w) = c.input["what"].as_str()
        {
            m.gaps.push(w.to_owned());
        }
        if WRITES.contains(&c.name.as_str()) {
            let hinted = serde_json::from_str::<Value>(&c.output)
                .ok()
                .and_then(|v| v["warnings"].as_array().cloned())
                .into_iter()
                .flatten()
                .any(|w| w.as_str().is_some_and(is_reuse_hint));
            if hinted {
                m.rebuilding_writes += 1;
                m.rebuilding_ms += c.ms;
            }
        }
    }
    m
}

/// A refused call's error in a line: `error: detail` from its JSON answer,
/// else the answer's first line.
fn refusal(output: &str) -> String {
    let line = match serde_json::from_str::<Value>(output) {
        Ok(v) if v.get("error").is_some() => {
            let e = v["error"]
                .as_str()
                .map_or(v["error"].to_string(), str::to_owned);
            match v["detail"].as_str().or(v["message"].as_str()) {
                Some(d) => format!("{e}: {d}"),
                None => e,
            }
        }
        _ => output.lines().next().unwrap_or("").to_owned(),
    };
    line.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built() -> Built {
        Built {
            layouts: vec![
                Layout {
                    id: "home".into(),
                    reached: Some("home".into()),
                    copy_of: Some("@core/shell".into()),
                    layout: json!({"type": "core://Page", "root": {
                        "type": "core://Layout",
                        "ref": {"name": "@core/checklist", "as": "x"},
                    }}),
                },
                Layout {
                    id: "log".into(),
                    reached: None,
                    copy_of: Some("@core/events".into()),
                    layout: json!({"root": {"type": "core://Use", "ref": "@me/row"}}),
                },
            ],
            components: vec![(
                "@me/row".into(),
                json!({"template": {"type": "core://Use", "ref": "@core/list_row/slot:x"}}),
            )],
        }
    }

    #[test]
    fn reads_references_and_reach() {
        let b = built();
        let r = b.references();
        assert!(r.contains("@core/checklist"));
        assert!(r.contains("@core/list_row"), "a part counts as its element");
        assert!(r.contains("@me/row"));
        assert_eq!(b.unreached(), vec!["log"]);
    }

    #[test]
    fn shipped_pieces_not_used_are_rebuilt() {
        let uses = ["@core/checklist", "@core/events", "@core/date_block"].map(String::from);
        let r = shipped_rebuilt(&uses, &built());
        assert_eq!(
            r,
            vec![
                Rebuilt {
                    name: "@core/events".into(),
                    how: "copied".into()
                },
                Rebuilt {
                    name: "@core/date_block".into(),
                    how: "not used".into()
                },
            ]
        );
    }

    #[test]
    fn counts_hinted_writes_refusals_and_gaps() {
        let call = |name: &str, input: Value, output: &str, is_error, ms| Call {
            name: name.into(),
            input,
            output: output.into(),
            is_error,
            ms,
        };
        let m = calls(&[
            call(
                "create_layout",
                json!({}),
                r#"{"ok":true,"warnings":["/root/0: this is what @core/list_row draws: use {...}"]}"#,
                false,
                4000,
            ),
            call("update_layout", json!({}), r#"{"ok":true}"#, false, 1000),
            call(
                "update_layout",
                json!({}),
                r#"{"error":"invalid"}"#,
                true,
                500,
            ),
            call(
                "note_gap",
                json!({"what": "a week strip"}),
                r#"{"ok":true}"#,
                false,
                10,
            ),
        ]);
        assert_eq!(m.tool_calls, 4);
        assert_eq!(m.rebuilding_writes, 1);
        assert_eq!(m.rebuilding_ms, 4000);
        assert_eq!(m.refused, vec!["update_layout: invalid"]);
        assert_eq!(m.gaps, vec!["a week strip"]);
    }
}
