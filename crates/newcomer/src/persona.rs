//! A persona: one person's wishes in their own words, and how to tell they
//! were met (decision `newcomer-journeys`). One markdown file each, in
//! `crates/newcomer/personas/`.

use std::path::Path;

/// Which pod a persona needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// A temporary pod: no model, web or search jacks.
    Temporary,
    /// A new admin pod, for wishes that need the model or the web.
    Fresh,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Persona {
    pub id: String,
    pub tier: Tier,
    /// User messages (wishes and answers) before the run counts as stuck.
    pub max_turns: usize,
    /// Another persona whose pod this one starts from (a later visit).
    pub starts_from: Option<String>,
    /// The agent's spending cap for the whole persona, in dollars.
    pub max_budget_usd: Option<f64>,
    pub who: String,
    /// The user's messages, in order.
    pub says: Vec<String>,
    /// Facts the simulated user may give when asked.
    pub if_asked: Vec<String>,
    /// The simulator script the result must pass (`sim:` lines joined).
    pub sim: String,
    /// What the server already ships for this ask: core layouts, kits'
    /// layouts and components the agent should use, not rebuild.
    pub uses: Vec<String>,
    /// Questions for the reviewer, each answered yes or no.
    pub judge: Vec<String>,
}

impl Persona {
    pub fn load(path: &Path) -> Result<Persona, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Persona::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Persona, String> {
        let text = text.trim_start();
        let rest = text
            .strip_prefix("---\n")
            .ok_or("a persona starts with a --- front matter block")?;
        let (front, body) = rest
            .split_once("\n---\n")
            .ok_or("the front matter has no closing ---")?;

        let mut p = Persona {
            id: String::new(),
            tier: Tier::Temporary,
            max_turns: 8,
            starts_from: None,
            max_budget_usd: None,
            who: String::new(),
            says: Vec::new(),
            if_asked: Vec::new(),
            sim: String::new(),
            uses: Vec::new(),
            judge: Vec::new(),
        };
        for line in front.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (k, v) = line
                .split_once(':')
                .ok_or_else(|| format!("front matter line without a colon: {line}"))?;
            let v = v.trim();
            match k.trim() {
                "id" => p.id = v.to_owned(),
                "tier" => {
                    p.tier = match v {
                        "temporary" => Tier::Temporary,
                        "fresh" => Tier::Fresh,
                        _ => return Err(format!("tier is temporary or fresh, not {v}")),
                    }
                }
                "max_turns" => p.max_turns = v.parse().map_err(|_| "max_turns is a number")?,
                "starts_from" => p.starts_from = Some(v.to_owned()),
                "max_budget_usd" => {
                    p.max_budget_usd = Some(v.parse().map_err(|_| "max_budget_usd is a number")?)
                }
                other => return Err(format!("unknown front matter key {other}")),
            }
        }
        if p.id.is_empty() {
            return Err("the front matter needs an id".into());
        }

        for (heading, section) in sections(body) {
            match heading.as_str() {
                "Who I am" => p.who = section.trim().to_owned(),
                "What I say" => p.says = numbered(&section),
                "If asked" => p.if_asked = bullets(&section),
                "Done when" => done_when(&section, &mut p)?,
                other => return Err(format!("unknown section ## {other}")),
            }
        }
        if p.says.is_empty() {
            return Err("## What I say has no numbered messages".into());
        }
        Ok(p)
    }
}

/// `## ` sections: (heading, body).
fn sections(body: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in body.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            out.push((h.trim().to_owned(), String::new()));
        } else if let Some((_, s)) = out.last_mut() {
            s.push_str(line);
            s.push('\n');
        }
    }
    out
}

/// `1. ...` items; an indented or unnumbered line continues the item above.
fn numbered(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in s.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let item = t
            .split_once(". ")
            .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        match (item, out.last_mut()) {
            (Some((_, text)), _) => out.push(text.trim().to_owned()),
            (None, Some(last)) => {
                last.push(' ');
                last.push_str(t);
            }
            (None, None) => {}
        }
    }
    out
}

fn bullets(s: &str) -> Vec<String> {
    s.lines()
        .filter_map(|l| l.trim().strip_prefix("- "))
        .map(|l| l.trim().to_owned())
        .collect()
}

fn done_when(s: &str, p: &mut Persona) -> Result<(), String> {
    let mut in_judge = false;
    let mut sim: Vec<String> = Vec::new();
    for line in s.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(v) = t.strip_prefix("sim:") {
            in_judge = false;
            sim.push(v.trim().to_owned());
        } else if let Some(v) = t.strip_prefix("uses:") {
            in_judge = false;
            p.uses.extend(
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            );
        } else if t == "judge:" {
            in_judge = true;
        } else if let (true, Some(j)) = (in_judge, t.strip_prefix("- ")) {
            p.judge.push(j.trim().to_owned());
        } else {
            return Err(format!("## Done when: unexpected line {t}"));
        }
    }
    p.sim = sim.join("; ");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GROCERIES: &str = "---
id: groceries
tier: temporary          # temporary | fresh
max_turns: 6
---
## Who I am
Shares a flat.

## What I say
1. I want a list
   called Groceries.
2. Ticked things below, please.

## If asked
- Quantities: sometimes.

## Done when
sim: shot home; assert-text Groceries
sim: shot end
uses: @core/checklist, @core/empty_state
judge:
- Home reaches the list.
- Items can be ticked.
";

    #[test]
    fn parses_a_persona() {
        let p = Persona::parse(GROCERIES).unwrap();
        assert_eq!(p.id, "groceries");
        assert_eq!(p.tier, Tier::Temporary);
        assert_eq!(p.max_turns, 6);
        assert_eq!(p.who, "Shares a flat.");
        assert_eq!(
            p.says,
            vec![
                "I want a list called Groceries.",
                "Ticked things below, please."
            ]
        );
        assert_eq!(p.if_asked, vec!["Quantities: sometimes."]);
        assert_eq!(p.sim, "shot home; assert-text Groceries; shot end");
        assert_eq!(p.uses, vec!["@core/checklist", "@core/empty_state"]);
        assert_eq!(p.judge.len(), 2);
    }

    #[test]
    fn refuses_unknown_keys_and_sections() {
        let bad = GROCERIES.replace("max_turns: 6", "turns: 6");
        assert!(
            Persona::parse(&bad)
                .unwrap_err()
                .contains("unknown front matter key")
        );
        let bad = GROCERIES.replace("## If asked", "## Maybe");
        assert!(
            Persona::parse(&bad)
                .unwrap_err()
                .contains("unknown section")
        );
    }

    #[test]
    fn the_shipped_personas_parse() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("personas");
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            if path.extension().is_some_and(|x| x == "md") {
                let p = Persona::load(&path).unwrap();
                assert_eq!(
                    Some(p.id.as_str()),
                    path.file_stem().and_then(|s| s.to_str()),
                    "a persona's id is its file name"
                );
                assert!(!p.sim.is_empty(), "{}: no sim: checks", p.id);
                n += 1;
            }
        }
        assert!(n > 0);
    }
}
