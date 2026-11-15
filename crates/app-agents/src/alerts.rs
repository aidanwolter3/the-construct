//! Notifications (decisions `agents-data-source`, `agents-cap-v3`, after
//! `server-monitoring-alerts`): the one layout that shows the source
//! (decision `notification-owner`) gets three channels, and each owed
//! [`Effect`] (from the runner's events) or [`Notice`] (from the pod's
//! schedule timer) becomes one notification there.

use pod_core::store::Store;
use pod_core::{Catalog, NewNotification};
use protocol::{Importance, NotificationChannel};

use crate::schedules::Notice;
use crate::state::{Effect, Run};
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
            "When a run waits for you (a permission, a question, a reply), and a reminder when a review waits a day",
            Importance::High,
        ),
        ch(
            SESSIONS,
            "Finished runs",
            "When a run's work is done (merged and deployed), or it ended needing your review",
            Importance::Default,
        ),
        ch(
            WARNINGS,
            "Runner warnings",
            "When a run is lost, doesn't start, runs too long, expires or fails; a sandbox goes down or keeps failing its disk; a scheduled run is missed; or a security check on the runner warns",
            Importance::High,
        ),
    ]
}

fn named(t: &str) -> String {
    if t.is_empty() {
        "a run".to_owned()
    } else {
        text::clean(t, 120)
    }
}

/// What a run did, and its pull request, as a notification's body.
fn did(run: &Run) -> String {
    let mut body = text::clean(run.said(), 300);
    if !run.pr().is_empty() {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(run.pr());
    }
    body
}

/// The notifications `effects` and the timer's `notices` owe in layout
/// `app`; `now` Unix seconds. Each one's dedupe key names its run (or
/// sandbox, or schedule) and transition.
pub fn notifications(
    effects: &[Effect],
    notices: &[Notice],
    app: &str,
    now: i64,
) -> Vec<NewNotification> {
    let n = |channel: &str,
             key: &str,
             title: String,
             body: String,
             target: Option<String>,
             quiet: bool| NewNotification {
        app_id: app.to_owned(),
        channel: channel.into(),
        dedupe_key: format!("agents:{key}"),
        title,
        body,
        image_url: None,
        target_uri: target,
        group_key: Some("agents".into()),
        created_at: now,
        quiet,
    };
    let target = |id: &str| Some(format!("agents://sessions/{id}"));
    let mut out = Vec::new();
    for e in effects {
        out.push(match e {
            Effect::NeedsYou { run, key } => n(
                NEEDS_YOU,
                key,
                format!("Needs you: {}", named(&run.title)),
                if run.needs_you_text.is_empty() {
                    format!("{}.", bundle::needs_you_label(&run.needs_you_reason))
                } else {
                    text::clean(&run.needs_you_text, 300)
                },
                target(&run.id),
                false,
            ),
            // Inbox only: a day-old review is a reminder, not an alarm.
            Effect::ReviewReminder { run, key } => n(
                NEEDS_YOU,
                key,
                format!("Still waiting for your review: {}", named(&run.title)),
                did(run),
                target(&run.id),
                true,
            ),
            Effect::Done { run, key } => n(
                SESSIONS,
                key,
                format!("Done: {}", named(&run.title)),
                did(run),
                target(&run.id),
                false,
            ),
            Effect::NeedsReview { run, key } => n(
                SESSIONS,
                key,
                format!("{}: {}", bundle::NEEDS_REVIEW, named(&run.title)),
                {
                    let b = did(run);
                    if b.is_empty() {
                        "It ended without a report or a pull request.".into()
                    } else {
                        b
                    }
                },
                target(&run.id),
                false,
            ),
            Effect::RunWarning { run, key } => {
                let (word, _) = bundle::run_look(run);
                let body = if run.outcome_state() == crate::state::OUTCOME_FAILED {
                    let d = did(run);
                    if d.is_empty() {
                        outcome_text(&run.outcome.reason).to_owned()
                    } else {
                        d
                    }
                } else if run.error.is_empty() {
                    end_reason_text(&run.end_reason).to_owned()
                } else {
                    text::clean(&run.error, 300)
                };
                n(
                    WARNINGS,
                    key,
                    format!("{word}: {}", named(&run.title)),
                    body,
                    target(&run.id),
                    false,
                )
            }
            Effect::SpriteWarning { sprite, disk, key } => n(
                WARNINGS,
                key,
                if *disk {
                    format!("Sandbox {} keeps failing its disk", sprite.name)
                } else {
                    format!("Sandbox {} is down", sprite.name)
                },
                if *disk {
                    format!(
                        "{} disk failures in 7 days: it likely needs replacing.",
                        sprite.disk_failures_7d
                    )
                } else if sprite.reason.is_empty() {
                    "Its runs wait until it is back; Repair sets it up again.".into()
                } else {
                    format!(
                        "{}. Its runs wait until it is back; Repair sets it up again.",
                        text::clean(&sprite.reason, 200).trim_end_matches('.')
                    )
                },
                None,
                false,
            ),
            Effect::CheckWarn {
                id,
                title,
                detail,
                key,
            } => n(
                WARNINGS,
                key,
                format!("Runner: {}", if title.is_empty() { id } else { title }),
                text::clean(detail, 300),
                None,
                false,
            ),
        });
    }
    for notice in notices {
        out.push(match notice {
            Notice::Missed {
                schedule,
                count,
                last,
                key,
            } => n(
                WARNINGS,
                key,
                format!("Missed: {}", text::clean(schedule, 120)),
                {
                    let last = crate::cron::shown(last);
                if *count == 1 {
                    format!("The run due {last} wasn't sent in time, so it was skipped.")
                } else {
                    format!("{count} runs (the last due {last}) weren't sent in time, so they were skipped.")
                }
                },
                None,
                false,
            ),
            Notice::Refused {
                schedule,
                occurrence,
                reason,
                key,
            } => n(
                WARNINGS,
                key,
                format!("Didn't start: {}", text::clean(schedule, 120)),
                format!(
                    "The runner refused the run due {}: {}",
                    crate::cron::shown(occurrence),
                    text::clean(reason, 200)
                ),
                None,
                false,
            ),
        });
    }
    out
}

/// Why an outcome failed, in words.
pub fn outcome_text(reason: &str) -> &'static str {
    match reason {
        "ci" => "Its pull request's checks failed.",
        "closed_unmerged" => "Its pull request was closed without merging.",
        "deploy" => "Its deploy failed or didn't come up healthy in time.",
        "blocked" => "The agent was blocked.",
        _ => "The agent reported that it failed.",
    }
}

/// Why a run ended badly, in words.
pub fn end_reason_text(reason: &str) -> &'static str {
    match reason {
        "lost" => "The run was lost: its process is gone.",
        "start_failed" => "The run didn't start.",
        "timed_out" => "The run passed its time limit and was stopped.",
        "expired" => "The run wasn't started in time.",
        "stopped" => "The run was stopped.",
        "reported" => "The run ended.",
        _ => "The run ended.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Claim, Outcome, Sprite};

    fn run(id: &str, end: &str, outcome: &str) -> Box<Run> {
        Box::new(Run {
            id: id.into(),
            title: "Fix login".into(),
            state: "ended".into(),
            end_reason: end.into(),
            claim: Some(Claim {
                status: "done".into(),
                summary: "Fixed the redirect.".into(),
                ..Default::default()
            }),
            outcome: Outcome {
                state: outcome.into(),
                pr_url: "https://github.com/owner/repo/pull/3".into(),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    #[test]
    fn effects_read_as_notifications() {
        let effects = [
            Effect::Done {
                run: run("run_a", "reported", "done"),
                key: "done:run_a".into(),
            },
            Effect::RunWarning {
                run: {
                    let mut r = run("run_b", "lost", "");
                    r.claim = None;
                    r
                },
                key: "ended:run_b".into(),
            },
            Effect::ReviewReminder {
                run: run("run_c", "reported", "pending"),
                key: "reminder:run_c:5".into(),
            },
            Effect::SpriteWarning {
                sprite: Box::new(Sprite {
                    name: "box-1".into(),
                    state: "down".into(),
                    reason: "No heartbeat for 15 min".into(),
                    ..Default::default()
                }),
                disk: false,
                key: "sprite_down:box-1:4".into(),
            },
        ];
        let notices = [Notice::Missed {
            schedule: "nightly".into(),
            count: 1,
            last: "2026-11-15T03:00".into(),
            key: "missed:sch_a:2026-11-15T03:00".into(),
        }];
        let got = notifications(&effects, &notices, "c-1", 100);
        assert_eq!(got.len(), 5);
        assert_eq!(got[0].title, "Done: Fix login");
        assert_eq!(got[0].channel, SESSIONS);
        assert_eq!(
            got[0].body,
            "Fixed the redirect.\nhttps://github.com/owner/repo/pull/3"
        );
        assert_eq!(got[0].dedupe_key, "agents:done:run_a");
        assert_eq!(
            (got[1].channel.as_str(), got[1].title.as_str()),
            (WARNINGS, "Lost: Fix login")
        );
        assert_eq!(got[1].body, "The run was lost: its process is gone.");
        assert!(got[2].quiet, "a reminder is inbox only");
        assert_eq!(got[2].channel, NEEDS_YOU);
        assert_eq!(got[3].title, "Sandbox box-1 is down");
        assert!(got[3].body.starts_with("No heartbeat for 15 min. "));
        assert_eq!(got[4].title, "Missed: nightly");
        assert!(!got.iter().take(2).any(|n| n.quiet));
    }
}
