//! The adapter's tables (decision `named-migrations`):
//! - `taste_account`: the service's URL and the pod's token for it, sealed
//!   ([`pod_core::secrets`]), with the like and skip counts it last gave;
//!   one row, or none when signed out.
//! - `taste_items`: the feed as the pod keeps it, in order: each item as the
//!   service sent it, with the reactions made since. The service answers a
//!   new page for every `/api/feed`, so the feed is fetched only when it is
//!   empty, on More and on Refresh, never on a data fetch.
//! - `taste_reactions`, `taste_served`, `taste_history`: the owner's taste
//!   history, which the pod owns and the service only caches (decision
//!   `taste-history-in-the-pod`): every reaction in order, how often each
//!   piece has been served, and the history's name (`log`, made once) and
//!   whether the service's own history has been taken over. Signing out
//!   keeps them: they're the owner's, not the service's.

use pod_core::store::{Step, Store};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use crate::api::Service;

const SCHEMA: &str = "
CREATE TABLE taste_account (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    url TEXT NOT NULL,
    token TEXT NOT NULL,
    likes INTEGER NOT NULL DEFAULT 0,
    skips INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    signed_in_at INTEGER NOT NULL
);
CREATE TABLE taste_items (
    pos INTEGER PRIMARY KEY,
    item_id TEXT NOT NULL UNIQUE,
    json TEXT NOT NULL
);
";

const HISTORY: &str = "
CREATE TABLE taste_reactions (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    item_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    t REAL NOT NULL
);
CREATE TABLE taste_served (
    item_id TEXT PRIMARY KEY,
    n INTEGER NOT NULL
);
CREATE TABLE taste_history (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    log TEXT NOT NULL,
    taken_over INTEGER NOT NULL DEFAULT 0
);
INSERT INTO taste_history (id, log) VALUES (1, lower(hex(randomblob(16))));
";

/// By name: add steps at the end; never rename, edit or remove one that
/// shipped.
pub static MIGRATIONS: &[Step] = &[Step::sql("schema", SCHEMA), Step::sql("history", HISTORY)];

const TOKEN_CONTEXT: &str = "taste_account.token:1";

/// The most items the feed keeps: the oldest go first past it.
pub const MAX_ITEMS: i64 = 240;
/// The most reacted items kept (the newest): the deck shows only the rest,
/// and lists use them only for their feed reasons.
pub const MAX_REACTED: i64 = 60;

/// The account: the service, its like and skip counts and the last error.
#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub service: Service,
    pub likes: i64,
    pub skips: i64,
    pub last_error: Option<String>,
}

pub fn account(store: &Store) -> rusqlite::Result<Option<Account>> {
    let row: Option<(String, String, i64, i64, Option<String>)> = store.with_conn(|c| {
        c.query_row(
            "SELECT url, token, likes, skips, last_error FROM taste_account WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
    })?;
    Ok(row.and_then(|(url, sealed, likes, skips, last_error)| {
        match store.secrets().open(TOKEN_CONTEXT, &sealed) {
            Ok(token) => Some(Account {
                service: Service::new(url, token),
                likes,
                skips,
                last_error,
            }),
            Err(e) => {
                tracing::error!(error = %e, "the taste service's token can't be opened");
                None
            }
        }
    }))
}

pub fn service(store: &Store) -> rusqlite::Result<Option<Service>> {
    Ok(account(store)?.map(|a| a.service))
}

pub fn signed_in(store: &Store) -> rusqlite::Result<bool> {
    store.with_conn(|c| {
        c.query_row("SELECT COUNT(*) FROM taste_account", [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|n| n > 0)
    })
}

/// Store `service` (sealing its token) with its counts, replacing whatever
/// was there, the feed included.
pub fn sign_in(
    store: &Store,
    service: &Service,
    likes: i64,
    skips: i64,
    now: i64,
) -> rusqlite::Result<()> {
    let sealed = store.secrets().seal(TOKEN_CONTEXT, &service.token);
    store.transaction(|tx| {
        tx.execute_batch("DELETE FROM taste_account; DELETE FROM taste_items;")?;
        tx.execute(
            "INSERT INTO taste_account (id, url, token, likes, skips, signed_in_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5)",
            params![service.url, sealed, likes, skips, now],
        )?;
        Ok(())
    })
}

/// Forget the service and the feed.
pub fn sign_out(store: &Store) -> rusqlite::Result<()> {
    store.transaction(|tx| tx.execute_batch("DELETE FROM taste_account; DELETE FROM taste_items;"))
}

/// Re-seal a token stored as plaintext or under an old key.
pub fn reseal(store: &Store) -> rusqlite::Result<()> {
    let sealed: Option<String> = store.with_conn(|c| {
        c.query_row("SELECT token FROM taste_account WHERE id = 1", [], |r| {
            r.get(0)
        })
        .optional()
    })?;
    let Some(sealed) = sealed else { return Ok(()) };
    if !store.secrets().needs_reseal(&sealed) {
        return Ok(());
    }
    let Ok(plain) = store.secrets().open(TOKEN_CONTEXT, &sealed) else {
        return Ok(());
    };
    let again = store.secrets().seal(TOKEN_CONTEXT, &plain);
    store.with_conn(|c| {
        c.execute("UPDATE taste_account SET token = ?1 WHERE id = 1", [again])
            .map(|_| ())
    })
}

pub fn set_counts(store: &Store, likes: i64, skips: i64) -> rusqlite::Result<()> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE taste_account SET likes = ?1, skips = ?2 WHERE id = 1",
            params![likes, skips],
        )
        .map(|_| ())
    })
}

/// Note how the last call to the service went (`None`: it answered).
pub fn set_error(store: &Store, error: Option<&str>) -> rusqlite::Result<()> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE taste_account SET last_error = ?1 WHERE id = 1",
            params![error],
        )
        .map(|_| ())
    })
}

/// The feed, in order.
pub fn items(store: &Store) -> rusqlite::Result<Vec<Value>> {
    store.with_conn(|c| {
        let mut st = c.prepare("SELECT json FROM taste_items ORDER BY pos")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            if let Ok(v) = serde_json::from_str(&row?) {
                out.push(v);
            }
        }
        Ok(out)
    })
}

/// Whether a kept item has been liked or skipped.
pub fn reacted(item: &Value) -> bool {
    item.get("liked").and_then(Value::as_bool) == Some(true)
        || item.get("skipped").and_then(Value::as_bool) == Some(true)
}

/// The kept items not reacted to yet, in order: the deck.
pub fn waiting(store: &Store) -> rusqlite::Result<Vec<Value>> {
    Ok(items(store)?.into_iter().filter(|i| !reacted(i)).collect())
}

/// One kept item by the service's id.
pub fn item(store: &Store, id: &str) -> rusqlite::Result<Option<Value>> {
    let raw: Option<String> = store.with_conn(|c| {
        c.query_row(
            "SELECT json FROM taste_items WHERE item_id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()
    })?;
    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
}

/// Add `page` after the feed (`replace`: instead of it). An item already
/// kept stays where it is. The oldest go past [`MAX_ITEMS`].
pub fn append(store: &Store, page: &[Value], replace: bool) -> rusqlite::Result<usize> {
    store.transaction(|tx| {
        if replace {
            tx.execute("DELETE FROM taste_items", [])?;
        }
        let mut added = 0;
        for item in page {
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                continue;
            };
            added += tx.execute(
                "INSERT OR IGNORE INTO taste_items (item_id, json) VALUES (?1, ?2)",
                params![id, item.to_string()],
            )?;
        }
        tx.execute(
            "DELETE FROM taste_items WHERE pos NOT IN
               (SELECT pos FROM taste_items ORDER BY pos DESC LIMIT ?1)",
            [MAX_ITEMS],
        )?;
        prune_reacted(tx)?;
        Ok(added)
    })
}

/// Drop reacted items past the newest [`MAX_REACTED`] of them.
fn prune_reacted(c: &rusqlite::Connection) -> rusqlite::Result<()> {
    let mut st = c.prepare("SELECT pos, json FROM taste_items ORDER BY pos DESC")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let mut seen = 0;
    let mut drop = Vec::new();
    for row in rows {
        let (pos, json) = row?;
        let reacted = serde_json::from_str::<Value>(&json).is_ok_and(|v| reacted(&v));
        if reacted {
            seen += 1;
            if seen > MAX_REACTED {
                drop.push(pos);
            }
        }
    }
    for pos in drop {
        c.execute("DELETE FROM taste_items WHERE pos = ?1", [pos])?;
    }
    Ok(())
}

/// Set a kept item's `liked` and `skipped` (a reaction the service took).
pub fn react(store: &Store, id: &str, liked: bool, skipped: bool) -> rusqlite::Result<()> {
    let Some(mut item) = self::item(store, id)? else {
        return Ok(());
    };
    item["liked"] = liked.into();
    item["skipped"] = skipped.into();
    store.with_conn(|c| {
        c.execute(
            "UPDATE taste_items SET json = ?1 WHERE item_id = ?2",
            params![item.to_string(), id],
        )
        .map(|_| ())
    })
}

/// The owner's taste history as a request names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History {
    /// Its name, made once per pod.
    pub log: String,
    /// The reactions in it: its version.
    pub version: i64,
    /// Whether the service's own history has been taken over (until then
    /// requests don't name the pod's).
    pub taken_over: bool,
}

impl History {
    /// `<log>/<version>`, the `X-Taste-State` header.
    pub fn tag(&self) -> String {
        format!("{}/{}", self.log, self.version)
    }
}

pub fn history(store: &Store) -> rusqlite::Result<History> {
    store.with_conn(|c| {
        let (log, taken_over): (String, bool) = c.query_row(
            "SELECT log, taken_over FROM taste_history WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let version = c.query_row("SELECT COUNT(*) FROM taste_reactions", [], |r| r.get(0))?;
        Ok(History {
            log,
            version,
            taken_over,
        })
    })
}

/// Add a reaction (`like`, `skip` or `clear`) at `t` (seconds); the history
/// after it.
pub fn record(store: &Store, id: &str, kind: &str, t: f64) -> rusqlite::Result<History> {
    store.with_conn(|c| {
        c.execute(
            "INSERT INTO taste_reactions (item_id, kind, t) VALUES (?1, ?2, ?3)",
            params![id, kind, t],
        )
        .map(|_| ())
    })?;
    history(store)
}

/// Take the newest reaction back out (the service didn't take it).
pub fn take_back_last(store: &Store) -> rusqlite::Result<()> {
    store.with_conn(|c| {
        c.execute(
            "DELETE FROM taste_reactions WHERE seq = (SELECT MAX(seq) FROM taste_reactions)",
            [],
        )
        .map(|_| ())
    })
}

/// Count `ids` as served once more each.
pub fn served(store: &Store, ids: &[String]) -> rusqlite::Result<()> {
    store.transaction(|tx| {
        for id in ids {
            tx.execute(
                "INSERT INTO taste_served (item_id, n) VALUES (?1, 1)
                 ON CONFLICT (item_id) DO UPDATE SET n = n + 1",
                [id],
            )?;
        }
        Ok(())
    })
}

/// The whole history, as `PUT /api/state` takes it: `{log, events: [{id,
/// kind, t}], served: {id: n}}`.
pub fn whole(store: &Store) -> rusqlite::Result<Value> {
    let h = history(store)?;
    store.with_conn(|c| {
        let mut st = c.prepare("SELECT item_id, kind, t FROM taste_reactions ORDER BY seq")?;
        let events = st
            .query_map([], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, String>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "t": r.get::<_, f64>(2)?,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut st = c.prepare("SELECT item_id, n FROM taste_served")?;
        let served = st
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, Value::from(r.get::<_, i64>(1)?)))
            })?
            .collect::<rusqlite::Result<serde_json::Map<String, Value>>>()?;
        Ok(serde_json::json!({"log": h.log, "events": events, "served": served}))
    })
}

/// Take over the service's own history (`GET /api/state` with no history
/// named: `{events, served}`) in place of the pod's, once. Reactions that
/// aren't `{id, kind, t}` are left out.
pub fn take_over(store: &Store, theirs: &Value) -> rusqlite::Result<History> {
    let events: Vec<(String, String, f64)> = theirs
        .get("events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let id = e.get("id")?.as_str()?.to_owned();
            let kind = e.get("kind")?.as_str()?.to_owned();
            let t = e.get("t")?.as_f64()?;
            matches!(kind.as_str(), "like" | "skip" | "clear" | "flag" | "unflag")
                .then_some((id, kind, t))
        })
        .collect();
    let served: Vec<(String, i64)> = theirs
        .get("served")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| Some((k.clone(), v.as_i64().filter(|n| *n > 0)?)))
        .collect();
    store.transaction(|tx| {
        tx.execute_batch("DELETE FROM taste_reactions; DELETE FROM taste_served;")?;
        for (id, kind, t) in &events {
            tx.execute(
                "INSERT INTO taste_reactions (item_id, kind, t) VALUES (?1, ?2, ?3)",
                params![id, kind, t],
            )?;
        }
        for (id, n) in &served {
            tx.execute(
                "INSERT INTO taste_served (item_id, n) VALUES (?1, ?2)",
                params![id, n],
            )?;
        }
        tx.execute("UPDATE taste_history SET taken_over = 1 WHERE id = 1", [])?;
        Ok(())
    })?;
    history(store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> Store {
        let store = Store::open_in_memory()
            .unwrap()
            .with_secrets(pod_core::secrets::Secrets::from_keys(&[[7u8; 32]]));
        pod_core::migrations::check_names("taste", MIGRATIONS).unwrap();
        store
            .transaction(|tx| {
                for step in MIGRATIONS {
                    match &step.run {
                        pod_core::migrations::Migration::Sql(sql) => tx.execute_batch(sql)?,
                        pod_core::migrations::Migration::Rust(f) => f(tx)?,
                    }
                }
                Ok(())
            })
            .unwrap();
        store
    }

    #[test]
    fn the_history_is_named_once_kept_and_taken_over_once() {
        let store = store();
        let h = history(&store).unwrap();
        assert_eq!(h.log.len(), 32);
        assert_eq!((h.version, h.taken_over), (0, false));
        let theirs = json!({
            "events": [{"id": "a", "kind": "like", "t": 1.0}, {"id": "b", "kind": "love", "t": 2.0},
                       {"id": "c", "kind": "skip", "t": 3.0}],
            "served": {"a": 2, "z": 0},
        });
        let h = take_over(&store, &theirs).unwrap();
        assert_eq!(
            (h.version, h.taken_over),
            (2, true),
            "a reaction of no known kind is left out"
        );
        let h = record(&store, "d", "like", 4.0).unwrap();
        assert_eq!(h.tag(), format!("{}/3", h.log));
        served(&store, &["a".into(), "d".into()]).unwrap();
        take_back_last(&store).unwrap();
        let all = whole(&store).unwrap();
        assert_eq!(all["log"], h.log);
        assert_eq!(all["events"].as_array().unwrap().len(), 2);
        assert_eq!(all["served"], json!({"a": 3, "d": 1}));
        // Signing out forgets the service, not the history.
        sign_in(&store, &Service::new("https://taste.example", "t"), 0, 0, 1).unwrap();
        sign_out(&store).unwrap();
        assert_eq!(history(&store).unwrap().version, 2);
    }

    #[test]
    fn the_token_is_sealed_and_signing_out_forgets_the_feed() {
        let store = store();
        let s = Service::new("https://taste.example", "secret-token");
        sign_in(&store, &s, 3, 4, 1).unwrap();
        let raw: String = store
            .with_conn(|c| c.query_row("SELECT token FROM taste_account", [], |r| r.get(0)))
            .unwrap();
        assert!(!raw.contains("secret-token"));
        let a = account(&store).unwrap().unwrap();
        assert_eq!((a.service, a.likes, a.skips), (s, 3, 4));
        append(&store, &[json!({"id": "a"})], false).unwrap();
        sign_out(&store).unwrap();
        assert!(account(&store).unwrap().is_none());
        assert!(items(&store).unwrap().is_empty());
    }

    #[test]
    fn pages_append_in_order_without_repeats_and_reactions_stick() {
        let store = store();
        append(&store, &[json!({"id": "a"}), json!({"id": "b"})], false).unwrap();
        let added = append(&store, &[json!({"id": "b"}), json!({"id": "c"})], false).unwrap();
        assert_eq!(added, 1);
        let ids: Vec<Value> = items(&store)
            .unwrap()
            .iter()
            .map(|i| i["id"].clone())
            .collect();
        assert_eq!(ids, vec![json!("a"), json!("b"), json!("c")]);
        react(&store, "b", true, false).unwrap();
        assert_eq!(item(&store, "b").unwrap().unwrap()["liked"], true);
        let waiting: Vec<Value> = waiting(&store)
            .unwrap()
            .iter()
            .map(|i| i["id"].clone())
            .collect();
        assert_eq!(waiting, vec![json!("a"), json!("c")]);
        append(&store, &[json!({"id": "d"})], true).unwrap();
        assert_eq!(items(&store).unwrap().len(), 1);
    }

    #[test]
    fn only_the_newest_reacted_items_stay() {
        let store = store();
        let page: Vec<Value> = (0..MAX_REACTED + 5)
            .map(|n| json!({"id": format!("r{n}"), "skipped": true}))
            .chain([json!({"id": "fresh"})])
            .collect();
        append(&store, &page, false).unwrap();
        let kept = items(&store).unwrap();
        assert_eq!(kept.len() as i64, MAX_REACTED + 1);
        assert_eq!(kept[0]["id"], "r5");
        assert_eq!(waiting(&store).unwrap().len(), 1);
    }

    #[test]
    fn the_feed_keeps_the_newest() {
        let store = store();
        let page: Vec<Value> = (0..MAX_ITEMS + 10)
            .map(|n| json!({"id": format!("i{n}")}))
            .collect();
        append(&store, &page, false).unwrap();
        let kept = items(&store).unwrap();
        assert_eq!(kept.len() as i64, MAX_ITEMS);
        assert_eq!(kept[0]["id"], "i10");
    }
}
