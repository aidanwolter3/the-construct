//! Notifications (decision `agents-data-source`, after
//! `server-monitoring-alerts`): the one layout that shows the source
//! (decision `notification-owner`) gets three channels, and each owed
//! [`Effect`] becomes one notification there.

use pod_core::store::Store;
use pod_core::{Catalog, NewNotification};
use protocol::{Importance, NotificationChannel};

use crate::state::Effect;
use crate::{bundle, text};

pub const NEEDS_YOU: &str = "needs_you";
pub const SESSIONS: &str = "sessions";
pub const WARNINGS: &str = "warnings";

/// The layout the channels belong to: the owner of the first of the
/// source's feeds some layout shows, while the pod is signed in to a
/// runner. Blocking.
pub fn owner(store: &Store, catalog: &Catalog) -> rusqlite::Result<Option<String>> {
    if crate::db::runner(store)?.is_none() {
        return Ok(None);
    }
    let mut owners = pod_core::custom::feed_owners(store, catalog, crate::NAME)?;
    Ok([
        crate::SESSIONS_FEED,
        crate::SCHEDULES_FEED,
        crate::CHECKS_FEED,
        crate::SANDBOXES_FEED,
    ]
    .iter()
    .find_map(|f| owners.remove(*f)))
}

pub fn channels(app: &str) -> Vec<NotificationChannel> {
    let ch = |id: &str, title: &str, description: &str, importance| NotificationChannel {
        app_id: app.to_owned(),
        id: id.into(),
        title: title.into(),
        description: description.into(),
        default_enabled: true,
        importance,
    };
    vec![
        ch(
            NEEDS_YOU,
            "Needs you",
            "When a run waits for you: a permission, a question, or a reply",
            Importance::High,
        ),
        ch(
            SESSIONS,
            "Finished runs",
            "When a run ends with a report (its summary and pull request)",
            Importance::Default,
        ),
        ch(
            WARNINGS,
            "Runner warnings",
            "When a run is lost, doesn't start, runs too long or expires, or a security check on the runner warns",
            Importance::High,
        ),
    ]
}

/// The notifications `effects` owe in layout `app`; `now` Unix seconds.
/// Each one's dedupe key names its run and transition.
pub fn notifications(effects: &[Effect], app: &str, now: i64) -> Vec<NewNotification> {
    let n = |channel: &str, key: String, title: String, body: String, target: Option<String>| {
        NewNotification {
            app_id: app.to_owned(),
            channel: channel.into(),
            dedupe_key: key,
            title,
            body,
            image_url: None,
            target_uri: target,
            group_key: Some("agents".into()),
            created_at: now,
            quiet: false,
        }
    };
    let named = |t: &str| {
        if t.is_empty() {
            "a run".to_owned()
        } else {
            text::clean(t, 120)
        }
    };
    let target = |id: &str| Some(format!("agents://sessions/{id}"));
    let mut out = Vec::new();
    for e in effects {
        out.push(match e {
            Effect::NeedsYou {
                run_id,
                title,
                reason,
                text: question,
                key,
            } => n(
                NEEDS_YOU,
                format!("agents:{key}"),
                format!("Needs you: {}", named(title)),
                if question.is_empty() {
                    format!("{}.", bundle::needs_you_label(reason))
                } else {
                    text::clean(question, 300)
                },
                target(run_id),
            ),
            Effect::Ended { run } => {
                let (word, _) = bundle::run_look(&run.state, &run.end_reason, &run.outcome);
                let bad = matches!(
                    run.end_reason.as_str(),
                    "lost" | "start_failed" | "timed_out" | "expired"
                ) && run.outcome.is_empty();
                if bad {
                    n(
                        WARNINGS,
                        format!("agents:ended:{}", run.id),
                        format!("{word}: {}", named(&run.title)),
                        if run.error.is_empty() {
                            end_reason_text(&run.end_reason).to_owned()
                        } else {
                            text::clean(&run.error, 300)
                        },
                        target(&run.id),
                    )
                } else if !run.outcome.is_empty() || run.end_reason == "reported" {
                    let summary = if run.outcome_summary.is_empty() {
                        &run.summary
                    } else {
                        &run.outcome_summary
                    };
                    let mut body = text::clean(summary, 300);
                    if !run.pr().is_empty() {
                        if !body.is_empty() {
                            body.push('\n');
                        }
                        body.push_str(run.pr());
                    }
                    n(
                        SESSIONS,
                        format!("agents:ended:{}", run.id),
                        format!("{word}: {}", named(&run.title)),
                        body,
                        target(&run.id),
                    )
                } else {
                    continue;
                }
            }
            Effect::CheckWarn {
                id,
                title,
                detail,
                key,
            } => n(
                WARNINGS,
                format!("agents:{key}"),
                format!("Runner: {}", if title.is_empty() { id } else { title }),
                text::clean(detail, 300),
                None,
            ),
        });
    }
    out
}

/// Why a run ended badly, in words.
pub fn end_reason_text(reason: &str) -> &'static str {
    match reason {
        "lost" => "The run was lost: its process is gone.",
        "start_failed" => "The run didn't start.",
        "timed_out" => "The run passed its time limit and was stopped.",
        "expired" => "The run wasn't started in time.",
        "stopped" => "The run was stopped.",
        "exited" => "The run ended without a report.",
        _ => "The run ended.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Run;

    #[test]
    fn effects_read_as_notifications() {
        let ended = |end: &str, outcome: &str| Effect::Ended {
            run: Box::new(Run {
                id: format!("run_{end}{outcome}"),
                title: "Fix login".into(),
                state: "ended".into(),
                end_reason: end.into(),
                outcome: outcome.into(),
                outcome_summary: "Fixed the redirect.".into(),
                outcome_pr_url: "https://github.com/owner/repo/pull/3".into(),
                ..Default::default()
            }),
        };
        let effects = [
            ended("reported", "review"),
            ended("stopped", ""),
            ended("lost", ""),
            Effect::NeedsYou {
                run_id: "run_1".into(),
                title: String::new(),
                reason: "permission".into(),
                text: String::new(),
                key: "needs:run_1:4".into(),
            },
        ];
        let got = notifications(&effects, "c-1", 100);
        assert_eq!(got.len(), 3, "a stopped run notifies nothing");
        assert_eq!(got[0].title, "Needs review: Fix login");
        assert_eq!(got[0].channel, SESSIONS);
        assert_eq!(
            got[0].body,
            "Fixed the redirect.\nhttps://github.com/owner/repo/pull/3"
        );
        assert_eq!(
            got[0].target_uri.as_deref(),
            Some("agents://sessions/run_reportedreview")
        );
        assert_eq!(got[1].channel, WARNINGS);
        assert_eq!(got[1].title, "Lost: Fix login");
        assert_eq!(got[1].body, "The run was lost: its process is gone.");
        assert_eq!(got[2].channel, NEEDS_YOU);
        assert_eq!(got[2].body, "Wants permission.");
        assert_eq!(got[2].dedupe_key, "agents:needs:run_1:4");
    }
}
