//! Comments this pod just posted, kept for a few minutes so the comments
//! screen lists them even before Pixelfed does.
//!
//! Pixelfed writes a reply at once, but its `GET /statuses/{id}/context`
//! lists descendants only when the parent's `replies_count` is above zero,
//! and that count is updated by a queued job. So the reload that follows a
//! first comment can come back without it, and the client's "Sending…" row
//! vanished until the screen was opened again.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::raw_status_id;

/// How long a posted comment is added to the screen when the upstream omits it.
pub const KEEP: Duration = Duration::from_secs(10 * 60);

/// Most comments kept per account.
const MAX_PER_ACCOUNT: usize = 50;

/// Raw statuses of recently posted comments, by `instance|account`.
#[derive(Default)]
pub struct RecentComments {
    inner: Mutex<HashMap<String, Vec<(Instant, Value)>>>,
}

fn owner_key(instance: &str, account: &str) -> String {
    format!("{instance}|{account}")
}

impl RecentComments {
    /// Remember `raw`, the upstream's answer to posting a comment.
    pub fn record(&self, instance: &str, account: &str, raw: Value) {
        self.record_at(instance, account, raw, Instant::now());
    }

    fn record_at(&self, instance: &str, account: &str, raw: Value, now: Instant) {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, list| {
            list.retain(|(at, _)| now.duration_since(*at) < KEEP);
            !list.is_empty()
        });
        let list = map.entry(owner_key(instance, account)).or_default();
        list.push((now, raw));
        if list.len() > MAX_PER_ACCOUNT {
            list.remove(0);
        }
    }

    /// Append to `context["descendants"]` the remembered comments under
    /// `post_id` (or under a comment listed there) that it does not list yet.
    pub fn merge_into(&self, instance: &str, account: &str, post_id: &str, context: &mut Value) {
        self.merge_at(instance, account, post_id, context, Instant::now());
    }

    fn merge_at(
        &self,
        instance: &str,
        account: &str,
        post_id: &str,
        context: &mut Value,
        now: Instant,
    ) {
        let mut pending: Vec<Value> = {
            let map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            match map.get(&owner_key(instance, account)) {
                Some(list) => list
                    .iter()
                    .filter(|(at, _)| now.duration_since(*at) < KEEP)
                    .map(|(_, raw)| raw.clone())
                    .collect(),
                None => return,
            }
        };
        if !context.is_object() {
            return;
        }
        if !context["descendants"].is_array() {
            context["descendants"] = Value::Array(Vec::new());
        }
        let Some(descendants) = context["descendants"].as_array_mut() else {
            return;
        };
        let mut ids: HashSet<String> = descendants.iter().filter_map(raw_status_id).collect();
        let mut added = 0;
        // Repeat so a reply to a just-posted comment finds its parent.
        loop {
            let before = pending.len();
            pending.retain(|raw| {
                let Some(id) = raw_status_id(raw) else {
                    return false;
                };
                if ids.contains(&id) {
                    return false; // the upstream lists it now
                }
                let parent = raw["in_reply_to_id"].as_str().unwrap_or_default();
                if parent == post_id || ids.contains(parent) {
                    ids.insert(id);
                    descendants.push(raw.clone());
                    added += 1;
                    return false;
                }
                true
            });
            if pending.len() == before {
                break;
            }
        }
        if added > 0 {
            tracing::info!(
                post_id,
                added,
                "added just-posted comments the upstream omits"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reply(id: &str, to: &str) -> Value {
        json!({ "id": id, "in_reply_to_id": to })
    }

    fn ids(context: &Value) -> Vec<String> {
        context["descendants"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(raw_status_id)
            .collect()
    }

    #[test]
    fn adds_a_comment_the_upstream_omits() {
        let r = RecentComments::default();
        r.record("https://pf.example", "1", reply("10", "5"));
        let mut ctx = json!({ "ancestors": [], "descendants": [] });
        r.merge_into("https://pf.example", "1", "5", &mut ctx);
        assert_eq!(ids(&ctx), ["10"]);
    }

    #[test]
    fn fills_in_missing_descendants() {
        let r = RecentComments::default();
        r.record("https://pf.example", "1", reply("10", "5"));
        let mut ctx = json!({ "ancestors": [] });
        r.merge_into("https://pf.example", "1", "5", &mut ctx);
        assert_eq!(ids(&ctx), ["10"]);
    }

    #[test]
    fn no_duplicate_once_the_upstream_lists_it() {
        let r = RecentComments::default();
        r.record("https://pf.example", "1", reply("10", "5"));
        let mut ctx = json!({ "descendants": [reply("9", "5"), reply("10", "5")] });
        r.merge_into("https://pf.example", "1", "5", &mut ctx);
        assert_eq!(ids(&ctx), ["9", "10"]);
    }

    #[test]
    fn only_this_post_and_this_account() {
        let r = RecentComments::default();
        r.record("https://pf.example", "1", reply("10", "5"));
        r.record("https://pf.example", "2", reply("11", "5"));
        r.record("https://pf.example", "1", reply("12", "6"));
        let mut ctx = json!({ "descendants": [] });
        r.merge_into("https://pf.example", "1", "5", &mut ctx);
        assert_eq!(ids(&ctx), ["10"]);
    }

    #[test]
    fn replies_to_listed_or_just_posted_comments() {
        let r = RecentComments::default();
        // Recorded child first: found on the second pass.
        r.record("https://pf.example", "1", reply("21", "20"));
        r.record("https://pf.example", "1", reply("20", "5"));
        r.record("https://pf.example", "1", reply("22", "9"));
        let mut ctx = json!({ "descendants": [reply("9", "5")] });
        r.merge_into("https://pf.example", "1", "5", &mut ctx);
        assert_eq!(ids(&ctx), ["9", "20", "22", "21"]);
    }

    #[test]
    fn forgets_after_a_while() {
        let r = RecentComments::default();
        let then = Instant::now();
        r.record_at("https://pf.example", "1", reply("10", "5"), then);
        let mut ctx = json!({ "descendants": [] });
        r.merge_at("https://pf.example", "1", "5", &mut ctx, then + KEEP);
        assert_eq!(ids(&ctx), Vec::<String>::new());
    }
}
