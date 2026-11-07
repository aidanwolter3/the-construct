//! Notifications (decision `agents-data-source`, after
//! `server-monitoring-alerts`): the one layout that shows the source
//! (decision `notification-owner`) gets three channels, and each owed
//! [`Effect`] becomes one notification there.

use pod_core::store::Store;
use pod_core::{Catalog, NewNotification};
use protocol::{Importance, NotificationChannel};

use crate::state::Effect;
use crate::text;

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
            "When a Claude session waits for your permission",
            Importance::High,
        ),
        ch(
            SESSIONS,
            "Finished sessions",
            "When a Claude session finishes (with its summary and pull request) or fails",
            Importance::Default,
        ),
        ch(
            WARNINGS,
            "Runner warnings",
            "When a security check on the agent runner warns, or a schedule is missed or runs too long",
            Importance::High,
        ),
    ]
}

/// The notifications `effects` owe in layout `app`; `seq` is the event (or
/// snapshot cursor) they came from, `now` Unix seconds.
pub fn notifications(effects: &[Effect], app: &str, seq: i64, now: i64) -> Vec<NewNotification> {
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
            "a session".to_owned()
        } else {
            text::clean(t, 120)
        }
    };
    let target = |id: &str| Some(format!("agents://sessions/{id}"));
    let mut out = Vec::new();
    for e in effects {
        out.push(match e {
            Effect::NeedsYou {
                session_id,
                title,
                detail,
            } => n(
                NEEDS_YOU,
                format!("agents:needs:{session_id}:{seq}"),
                format!("Needs you: {}", named(title)),
                if detail.is_empty() {
                    "A session is waiting for your permission.".to_owned()
                } else {
                    format!("Wants to use {detail}")
                },
                target(session_id),
            ),
            Effect::Finished {
                session_id,
                title,
                summary,
                pr_url,
            } => {
                let mut body = text::clean(summary, 300);
                if !pr_url.is_empty() {
                    if !body.is_empty() {
                        body.push('\n');
                    }
                    body.push_str(pr_url);
                }
                n(
                    SESSIONS,
                    format!("agents:finished:{session_id}"),
                    format!("Finished: {}", named(title)),
                    body,
                    target(session_id),
                )
            }
            Effect::Failed {
                session_id,
                title,
                error,
            } => n(
                SESSIONS,
                format!("agents:failed:{session_id}"),
                format!("Failed: {}", named(title)),
                text::clean(error, 300),
                target(session_id),
            ),
            Effect::CheckWarn { id, title, detail } => n(
                WARNINGS,
                format!("agents:check:{id}:{seq}"),
                format!("Runner: {}", if title.is_empty() { id } else { title }),
                text::clean(detail, 300),
                None,
            ),
            Effect::ScheduleMissed { name, error } => n(
                WARNINGS,
                format!("agents:missed:{name}:{seq}"),
                format!("Schedule missed: {name}"),
                text::clean(error, 300),
                Some(format!("agents://schedules/{name}")),
            ),
            Effect::ScheduleTimeout {
                name,
                minutes,
                stopped,
            } => n(
                WARNINGS,
                format!("agents:timeout:{name}:{seq}"),
                format!("Schedule ran too long: {name}"),
                format!(
                    "It passed its {minutes}-minute limit{}",
                    if *stopped { " and was stopped." } else { "." }
                ),
                Some(format!("agents://schedules/{name}")),
            ),
            _ => continue,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_read_as_notifications() {
        let effects = [
            Effect::Finished {
                session_id: "s1".into(),
                title: "Fix login".into(),
                summary: "Fixed the redirect.".into(),
                pr_url: "https://github.com/owner/repo/pull/3".into(),
            },
            Effect::TaskStarted {
                task_id: "r1".into(),
                session_id: "s1".into(),
            },
            Effect::ScheduleTimeout {
                name: "nightly".into(),
                minutes: 30,
                stopped: true,
            },
        ];
        let got = notifications(&effects, "c-1", 7, 100);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].title, "Finished: Fix login");
        assert_eq!(
            got[0].body,
            "Fixed the redirect.\nhttps://github.com/owner/repo/pull/3"
        );
        assert_eq!(got[0].channel, SESSIONS);
        assert_eq!(got[0].target_uri.as_deref(), Some("agents://sessions/s1"));
        assert_eq!(got[1].dedupe_key, "agents:timeout:nightly:7");
        assert_eq!(
            got[1].body,
            "It passed its 30-minute limit and was stopped."
        );
    }
}
