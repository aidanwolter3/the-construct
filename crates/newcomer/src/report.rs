//! A run's report: `report.json` (what the daily session records in the
//! owner's pod) and `report.md` (to read).

use crate::measure::Rebuilt;
use crate::persona::Tier;
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// Hard checks pass, nothing shipped was rebuilt, the reviewer said yes.
    Pass,
    /// Hard checks pass but the reviewer said no to a line: a fail only
    /// when it happens two days running.
    Flagged,
    /// A hard check failed or a shipped piece was rebuilt.
    Fail,
    /// The turns or the budget ran out, or the agent gave up.
    Stuck,
    /// Not the flow's fault: the server, the pods or the model failed.
    Broken,
    /// Not run (a fresh-tier persona without a fresh pod).
    Skipped,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Answer {
    pub line: String,
    pub yes: bool,
    pub why: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PersonaReport {
    pub id: String,
    pub tier: Tier,
    pub result: Outcome,
    /// Why, in a line, when it isn't a pass.
    pub reason: String,
    pub pod: String,
    /// Messages the user sent (wishes and answers).
    pub turns: usize,
    pub tool_calls: usize,
    pub refused: Vec<String>,
    pub gaps: Vec<String>,
    pub shipped_rebuilt: Vec<Rebuilt>,
    pub hints_left: Vec<String>,
    pub rebuilding_writes: usize,
    pub rebuilding_secs: f64,
    /// The agent's time until the checks passed; `None` when they didn't.
    pub time_to_working_secs: Option<f64>,
    pub agent_secs: f64,
    pub cost_usd: f64,
    pub sim_passed: bool,
    pub sim_output: String,
    pub unreached: Vec<String>,
    pub judge: Vec<Answer>,
    pub screenshots: Vec<String>,
    pub last_reply: String,
    pub model: String,
    pub claude_version: String,
}

impl PersonaReport {
    pub fn new(id: &str, tier: Tier) -> PersonaReport {
        PersonaReport {
            id: id.to_owned(),
            tier,
            result: Outcome::Broken,
            reason: String::new(),
            pod: String::new(),
            turns: 0,
            tool_calls: 0,
            refused: Vec::new(),
            gaps: Vec::new(),
            shipped_rebuilt: Vec::new(),
            hints_left: Vec::new(),
            rebuilding_writes: 0,
            rebuilding_secs: 0.0,
            time_to_working_secs: None,
            agent_secs: 0.0,
            cost_usd: 0.0,
            sim_passed: false,
            sim_output: String::new(),
            unreached: Vec::new(),
            judge: Vec::new(),
            screenshots: Vec::new(),
            last_reply: String::new(),
            model: String::new(),
            claude_version: String::new(),
        }
    }

    /// The result from the checks, in order of weight.
    pub fn decide(&mut self, stuck: Option<String>) {
        let (result, reason) = if let Some(why) = stuck {
            (Outcome::Stuck, why)
        } else if !self.sim_passed {
            (
                Outcome::Fail,
                format!("simulator check failed: {}", last_line(&self.sim_output)),
            )
        } else if !self.shipped_rebuilt.is_empty() {
            let names: Vec<String> = self
                .shipped_rebuilt
                .iter()
                .map(|r| format!("{} ({})", r.name, r.how))
                .collect();
            (
                Outcome::Fail,
                format!("rebuilt what the server ships: {}", names.join(", ")),
            )
        } else if !self.unreached.is_empty() {
            (
                Outcome::Fail,
                format!("home doesn't reach {}", self.unreached.join(", ")),
            )
        } else if let Some(no) = self.judge.iter().find(|a| !a.yes) {
            (
                Outcome::Flagged,
                format!("reviewer said no: {} ({})", no.line, no.why),
            )
        } else {
            (Outcome::Pass, String::new())
        };
        self.result = result;
        self.reason = reason;
    }
}

fn last_line(s: &str) -> &str {
    s.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("")
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Report {
    pub date: String,
    pub base_url: String,
    pub personas: Vec<PersonaReport>,
}

impl Report {
    pub fn markdown(&self) -> String {
        let mut s = format!(
            "# Newcomer test, {}\n\nAgainst {}.\n\n",
            self.date, self.base_url
        );
        s.push_str(
            "| Persona | Result | Turns | Tool calls | Refused | Rebuilt | Time to working | Cost |\n",
        );
        s.push_str("| --- | --- | --- | --- | --- | --- | --- | --- |\n");
        for p in &self.personas {
            let _ = writeln!(
                s,
                "| {} | {:?} | {} | {} | {} | {} | {} | ${:.2} |",
                p.id,
                p.result,
                p.turns,
                p.tool_calls,
                p.refused.len(),
                p.shipped_rebuilt.len() + p.hints_left.len(),
                p.time_to_working_secs
                    .map_or("-".to_owned(), |t| format!("{t:.0} s")),
                p.cost_usd,
            );
        }
        for p in &self.personas {
            let _ = write!(s, "\n## {}: {:?}\n\n", p.id, p.result);
            if !p.reason.is_empty() {
                let _ = writeln!(s, "{}\n", p.reason);
            }
            list(
                &mut s,
                "Shipped pieces rebuilt",
                p.shipped_rebuilt
                    .iter()
                    .map(|r| format!("{} ({})", r.name, r.how)),
            );
            list(&mut s, "Other rebuilt pieces", p.hints_left.iter().cloned());
            list(&mut s, "Refused calls", p.refused.iter().cloned());
            list(&mut s, "Gaps", p.gaps.iter().cloned());
            list(
                &mut s,
                "Reviewer",
                p.judge
                    .iter()
                    .map(|a| format!("{}: {} {}", if a.yes { "yes" } else { "NO" }, a.line, a.why)),
            );
        }
        s
    }
}

fn list(s: &mut String, title: &str, items: impl Iterator<Item = String>) {
    let items: Vec<String> = items.collect();
    if items.is_empty() {
        return;
    }
    let _ = writeln!(s, "{title}:\n");
    for i in items {
        let _ = writeln!(s, "- {}", i.replace('\n', " "));
    }
    s.push('\n');
}

/// `YYYY-MM-DD` of a Unix time, UTC.
pub fn date(unix_secs: u64) -> String {
    // Civil from days (Howard Hinnant's algorithm).
    let z = (unix_secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_795_881_600), "2026-11-28");
    }

    #[test]
    fn decides_in_order_of_weight() {
        let mut p = PersonaReport::new("g", Tier::Temporary);
        p.sim_passed = true;
        p.judge.push(Answer {
            line: "Ticked below".into(),
            yes: false,
            why: "they stay".into(),
        });
        p.decide(None);
        assert_eq!(p.result, Outcome::Flagged);

        p.shipped_rebuilt.push(Rebuilt {
            name: "@core/checklist".into(),
            how: "not used".into(),
        });
        p.decide(None);
        assert_eq!(p.result, Outcome::Fail);
        assert!(p.reason.contains("@core/checklist (not used)"));

        p.sim_passed = false;
        p.sim_output = "step 3: no target Add\n".into();
        p.decide(None);
        assert!(p.reason.ends_with("no target Add"));

        p.decide(Some("8 turns used".into()));
        assert_eq!(p.result, Outcome::Stuck);
    }

    #[test]
    fn markdown_has_a_row_per_persona() {
        let mut p = PersonaReport::new("groceries", Tier::Temporary);
        p.sim_passed = true;
        p.time_to_working_secs = Some(42.4);
        p.decide(None);
        let r = Report {
            date: "2026-11-28".into(),
            base_url: "https://pod.example".into(),
            personas: vec![p],
        };
        let md = r.markdown();
        assert!(md.contains("| groceries | Pass | 0 | 0 | 0 | 0 | 42 s | $0.00 |"));
    }
}
