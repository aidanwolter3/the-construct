//! Rules: which events are worth a notification. Declarative only:
//! keywords (any of, all of, none of), a venue, days of the week, a date
//! window and the targets it applies to. Matching ignores case and runs
//! over the event's title, venue and description.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::when::{DAY_NAMES, When};

/// A rule's conditions; every set condition must hold.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSpec {
    /// At least one of these phrases.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub any: Vec<String>,
    /// Every one of these phrases.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all: Vec<String>,
    /// None of these phrases ("class", "workshop").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub none: Vec<String>,
    /// The venue contains this.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub venue: String,
    /// `mon` … `sun`; empty: any day.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub days: Vec<String>,
    /// Only events starting within this many days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within_days: Option<u32>,
    /// Target ids it applies to; empty: every target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
}

/// What a rule is matched against.
pub struct Candidate<'a> {
    pub target_id: &'a str,
    pub title: &'a str,
    pub venue: &'a str,
    pub description: &'a str,
    /// The source's wall clock (for the day of the week).
    pub start: When,
    pub start_ts: i64,
}

/// Lowercase with whitespace runs as one space.
pub fn normalize(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl RuleSpec {
    pub fn matches(&self, c: &Candidate<'_>, now: i64) -> bool {
        if !self.targets.is_empty() && !self.targets.iter().any(|t| t == c.target_id) {
            return false;
        }
        let text = normalize(&format!("{}\n{}\n{}", c.title, c.venue, c.description));
        let has = |p: &String| text.contains(&normalize(p));
        if !self.any.is_empty() && !self.any.iter().any(has) {
            return false;
        }
        if !self.all.iter().all(has) || self.none.iter().any(has) {
            return false;
        }
        if !self.venue.trim().is_empty() && !normalize(c.venue).contains(&normalize(&self.venue)) {
            return false;
        }
        if !self.days.is_empty() && !self.days.iter().any(|d| d == DAY_NAMES[c.start.weekday()]) {
            return false;
        }
        if let Some(n) = self.within_days
            && c.start_ts > now + i64::from(n) * 86_400
        {
            return false;
        }
        true
    }

    /// One line for people: `"budget cuts" · not class · Fri, Sat`.
    pub fn summary(&self) -> String {
        let q = |v: &[String]| {
            v.iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(" or ")
        };
        let mut parts = Vec::new();
        if !self.any.is_empty() {
            parts.push(q(&self.any));
        }
        if !self.all.is_empty() {
            parts.push(format!(
                "all of {}",
                self.all
                    .iter()
                    .map(|p| format!("\"{p}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !self.none.is_empty() {
            parts.push(format!("not {}", q(&self.none)));
        }
        if !self.venue.is_empty() {
            parts.push(format!("at {}", self.venue));
        }
        if !self.days.is_empty() {
            let mut days: Vec<String> = Vec::new();
            for d in &self.days {
                let mut c = d.chars();
                if let Some(f) = c.next() {
                    days.push(f.to_uppercase().chain(c).collect());
                }
            }
            parts.push(days.join(", "));
        }
        if let Some(n) = self.within_days {
            parts.push(format!("within {n} days"));
        }
        if !self.targets.is_empty() {
            let n = self.targets.len();
            parts.push(format!("{n} site{}", if n == 1 { "" } else { "s" }));
        }
        if parts.is_empty() {
            "every event".to_owned()
        } else {
            parts.join(" · ")
        }
    }
}

/// A list of phrases from an array, or from text a form sends
/// (`"budget cuts, house team"`).
fn phrases(v: &Value, key: &str) -> Result<Option<Vec<String>>, String> {
    let list: Vec<String> = match v {
        Value::Null => return Ok(None),
        Value::String(s) => s.split(',').map(str::to_owned).collect(),
        Value::Array(a) => a
            .iter()
            .map(|x| x.as_str().map(str::to_owned))
            .collect::<Option<_>>()
            .ok_or(format!("\"{key}\" is a list of text"))?,
        _ => return Err(format!("\"{key}\" is a list of text")),
    };
    let list: Vec<String> = list
        .iter()
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect();
    if list.len() > 20 || list.iter().any(|p| p.chars().count() > 100) {
        return Err(format!("\"{key}\": at most 20 phrases of 100 characters"));
    }
    Ok(Some(list))
}

/// Apply the condition fields present in `body` to `spec` (a create
/// starts from the default). Lenient the way forms send values.
pub fn apply(spec: &mut RuleSpec, body: &Value) -> Result<(), String> {
    // The app's form names: "keywords" (any of) and "exclude" (none of).
    for (keys, slot) in [
        (["any", "keywords"], &mut spec.any),
        (["all", "all"], &mut spec.all),
        (["none", "exclude"], &mut spec.none),
    ] {
        for k in keys {
            if let Some(list) = phrases(&body[k], k)? {
                *slot = list;
            }
        }
    }
    if let Some(v) = body.get("venue") {
        spec.venue = v.as_str().ok_or("\"venue\" is text")?.trim().to_owned();
    }
    if let Some(list) = phrases(&body["days"], "days")? {
        let mut days = Vec::new();
        for d in list {
            let d3: String = d.to_lowercase().chars().take(3).collect();
            if !DAY_NAMES.contains(&d3.as_str()) {
                return Err(format!("\"{d}\" is not a day (mon … sun)"));
            }
            if !days.contains(&d3) {
                days.push(d3);
            }
        }
        spec.days = days;
    }
    match body.get("within_days") {
        None => {}
        Some(Value::Null) => spec.within_days = None,
        Some(Value::String(s)) if s.trim().is_empty() => spec.within_days = None,
        Some(v) => {
            let n = v
                .as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                .filter(|n| (1..=366).contains(n))
                .ok_or("\"within_days\" is a number of days (1-366)")?;
            spec.within_days = Some(n as u32);
        }
    }
    if let Some(list) = phrases(&body["targets"], "targets")? {
        spec.targets = list;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cand<'a>(title: &'a str, description: &'a str) -> Candidate<'a> {
        let start = When::parse("2026-11-06T19:30").unwrap(); // a Friday
        Candidate {
            target_id: "t1",
            title,
            venue: "The End Lafayette",
            description,
            start,
            start_ts: start.timestamp(None),
        }
    }

    #[test]
    fn keywords_and_negations() {
        let mut r = RuleSpec::default();
        apply(
            &mut r,
            &json!({"keywords": "Budget Cuts, house team", "exclude": ["class"]}),
        )
        .unwrap();
        let now = cand("", "").start_ts - 86_400;
        assert!(r.matches(
            &cand("Friday Improv", "Lineup: BUDGET  cuts, Sandwich"),
            now
        ));
        assert!(!r.matches(&cand("Improv class", "with budget cuts"), now));
        assert!(!r.matches(&cand("Friday Improv", "Lineup: others"), now));
        apply(&mut r, &json!({"days": ["Sat"]})).unwrap();
        assert!(!r.matches(&cand("x", "budget cuts"), now));
        apply(
            &mut r,
            &json!({"days": "fri", "within_days": "1", "venue": "the end"}),
        )
        .unwrap();
        assert!(!r.matches(&cand("x", "budget cuts"), now - 86_400));
        assert!(r.matches(&cand("x", "budget cuts"), now));
        assert_eq!(
            r.summary(),
            "\"Budget Cuts\" or \"house team\" · not \"class\" · at the end · Fri · within 1 days"
        );
        assert!(apply(&mut r, &json!({"days": "someday"})).is_err());
        assert_eq!(RuleSpec::default().summary(), "every event");
    }

    #[test]
    fn targets_scope() {
        let mut r = RuleSpec::default();
        apply(&mut r, &json!({"targets": ["t2"]})).unwrap();
        assert!(!r.matches(&cand("x", ""), 0));
    }
}
