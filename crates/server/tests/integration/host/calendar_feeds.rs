//! Decision `ical-feeds`: a pod's calendar feed, made from Collections rows
//! after a review, served by the host at `/ical/<token>.ics` to anyone with
//! the URL (a calendar app), never reaching the pod; kept current as rows
//! change; revoked and rotated at once.

use super::*;
use server::host::web::WebConfig;

/// Calls a pod MCP tool with an MCP credential: its JSON answer.
async fn tool(env: &Env, key: &str, name: &str, args: Value) -> Value {
    let resp: Value = client()
        .post(env.url("/v1/mcp"))
        .bearer_auth(key)
        .header("accept", "application/json, text/event-stream")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("null");
    serde_json::from_str(text).unwrap_or(resp)
}

async fn mcp_key(env: &Env, pod: &str) -> String {
    let (host, pod) = (env.host.clone(), pod.to_owned());
    tokio::task::spawn_blocking(move || {
        host.issue_mcp(&pod, "test", pod_core::mcp::Access::Outward)
            .unwrap()
    })
    .await
    .unwrap()
}

/// A server that serves public links (and so calendar feeds).
async fn public_env() -> (Env, tempfile::TempDir) {
    let files = tempfile::tempdir().unwrap();
    std::fs::write(files.path().join("index.html"), "<title>t</title>").unwrap();
    let env = env_with(HostConfig {
        web: Some(WebConfig {
            dir: files.path().to_owned(),
        }),
        public_web: true,
        ..HostConfig::default()
    })
    .await;
    (env, files)
}

/// `GET` a feed: its status, content type and text.
async fn fetch(env: &Env, url: &str) -> (StatusCode, String, String) {
    let path = url.find("/ical/").map_or(url, |i| &url[i..]);
    let r = client().get(env.url(path)).send().await.unwrap();
    let ty = r
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    (r.status(), ty, r.text().await.unwrap())
}

/// The feed's text unfolded (continuation lines joined back).
fn unfold(ics: &str) -> String {
    ics.replace("\r\n ", "")
}

/// The `VEVENT` blocks whose text contains `needle`.
fn events_with<'a>(ics: &'a str, needle: &str) -> Vec<&'a str> {
    ics.split("BEGIN:VEVENT")
        .skip(1)
        .filter(|e| e.contains(needle))
        .collect()
}

fn line<'a>(event: &'a str, name: &str) -> &'a str {
    event
        .split("\r\n")
        .find(|l| l.starts_with(name))
        .unwrap_or_else(|| panic!("no {name} in {event}"))
}

async fn define(env: &Env, key: &str, name: &str, fields: Value) {
    let r = tool(
        env,
        key,
        "define_collection",
        json!({ "name": name, "fields": fields }),
    )
    .await;
    assert!(r.get("error").is_none(), "{r}");
}

async fn add(env: &Env, key: &str, collection: &str, values: Value) -> String {
    let r = tool(
        env,
        key,
        "add_row",
        json!({ "collection": collection, "values": values }),
    )
    .await;
    r["id"].as_str().unwrap_or_else(|| panic!("{r}")).to_owned()
}

/// The sources of the feed in these tests, as the user's first one.
fn sources() -> Value {
    json!([
        { "collection": "focus_days", "start": "date", "title": "{area} day" },
        { "collection": "workouts", "start": "date", "time": "time", "end_time": "ends",
          "title": "{kind}" },
        { "collection": "outings", "where": { "field": "going", "equals": true },
          "start": "start", "end": "end", "title": "{title}", "location": "{venue}",
          "description": "{blurb}" },
        { "collection": "one_offs", "start": "start", "end": "end", "title": "{title}",
          "location": "{location}", "description": "{notes}" },
    ])
}

#[tokio::test]
async fn a_calendar_feed_is_off_where_public_links_are() {
    let env = env().await;
    env.create("off").await;
    let key = mcp_key(&env, "off").await;
    let r = tool(
        &env,
        &key,
        "set_calendar_feed",
        json!({ "name": "mine", "sources": sources() }),
    )
    .await;
    assert_eq!(r["error"], "public_links_off", "{r}");
    let (status, _, _) = fetch(&env, "/ical/cal-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ics").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn a_calendar_feed_carries_only_what_its_sources_map_and_follows_edits() {
    let (env, _files) = public_env().await;
    env.create("plans").await;
    let key = mcp_key(&env, "plans").await;
    let tz = tool(
        &env,
        &key,
        "set_time_zone",
        json!({ "time_zone": "America/Denver" }),
    )
    .await;
    assert!(tz.get("error").is_none(), "{tz}");

    define(
        &env,
        &key,
        "focus_days",
        json!([{ "name": "date", "type": "date" }, { "name": "area", "type": "text" },
               { "name": "private_note", "type": "text", "optional": true }]),
    )
    .await;
    define(
        &env,
        &key,
        "workouts",
        json!([{ "name": "date", "type": "date" }, { "name": "kind", "type": "text" },
               { "name": "time", "type": "text", "optional": true },
               { "name": "ends", "type": "text", "optional": true }]),
    )
    .await;
    define(
        &env,
        &key,
        "outings",
        json!([{ "name": "title", "type": "text" }, { "name": "start", "type": "datetime" },
               { "name": "end", "type": "datetime", "optional": true },
               { "name": "venue", "type": "text", "optional": true },
               { "name": "blurb", "type": "text", "optional": true },
               { "name": "secret", "type": "text", "optional": true },
               { "name": "going", "type": "bool" }]),
    )
    .await;
    // A plain collection filled by hand (no filler): one-off events.
    define(
        &env,
        &key,
        "one_offs",
        json!([{ "name": "title", "type": "text" }, { "name": "start", "type": "datetime" },
               { "name": "end", "type": "datetime", "optional": true },
               { "name": "location", "type": "text", "optional": true },
               { "name": "notes", "type": "text", "optional": true }]),
    )
    .await;

    add(
        &env,
        &key,
        "focus_days",
        json!({ "date": "2030-03-09", "area": "House", "private_note": "NOTE-NEVER-SHARED" }),
    )
    .await;
    // Long past: older than 30 days, left out.
    add(
        &env,
        &key,
        "focus_days",
        json!({ "date": "2020-01-04", "area": "Attic" }),
    )
    .await;
    // Before daylight saving starts (2030-03-10 in Denver): UTC-7.
    add(
        &env,
        &key,
        "workouts",
        json!({ "date": "2030-03-08", "kind": "Group class", "time": "8:00 AM", "ends": "09:00" }),
    )
    .await;
    // After: UTC-6. A 12-hour start, a 24-hour end.
    add(
        &env,
        &key,
        "workouts",
        json!({ "date": "2030-03-12", "kind": "Swim", "time": "3:00 PM", "ends": "15:45" }),
    )
    .await;
    // No time: all day.
    add(
        &env,
        &key,
        "workouts",
        json!({ "date": "2030-03-16", "kind": "Hike" }),
    )
    .await;
    let blurb = "An evening of roller skating with a live band, food trucks, and a costume \
        contest; bring your own skates or rent a pair at the door.\nAll ages.";
    add(
        &env,
        &key,
        "outings",
        json!({ "title": "Roller night", "start": "2030-06-01T19:00:00-06:00",
                "venue": "Rink, Main St; Lot B \\ north", "blurb": blurb,
                "secret": "SECRET-NEVER-SHARED", "going": true }),
    )
    .await;
    add(
        &env,
        &key,
        "outings",
        json!({ "title": "Not going to this", "start": "2030-06-02T19:00:00-06:00",
                "going": false }),
    )
    .await;
    let skate = add(
        &env,
        &key,
        "one_offs",
        json!({ "title": "Evening skate", "start": "2030-12-03T18:30", "end": "2030-12-03T20:30",
                "location": "Pond Rink Example", "notes": "Bring gloves" }),
    )
    .await;

    // A source naming a field that isn't there is refused, with where.
    let bad = tool(
        &env,
        &key,
        "set_calendar_feed",
        json!({ "name": "mine",
        "sources": [{ "collection": "workouts", "start": "when", "title": "{kind}" }] }),
    )
    .await;
    assert_eq!(bad["error"], "invalid_source", "{bad}");
    assert_eq!(bad["pointer"], "/sources/0/start", "{bad}");
    let bad = tool(
        &env,
        &key,
        "set_calendar_feed",
        json!({ "name": "mine",
        "sources": [{ "collection": "workouts", "start": "date", "title": "{nope}" }] }),
    )
    .await;
    assert_eq!(bad["pointer"], "/sources/0/title", "{bad}");

    // The review: every text a subscriber reads, and nothing is published.
    let review = tool(
        &env,
        &key,
        "set_calendar_feed",
        json!({ "name": "mine", "title": "My plans", "sources": sources() }),
    )
    .await;
    let id = review["review_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{review}"))
        .to_owned();
    let reads = &review["a_subscriber_reads"];
    assert_eq!(
        reads["titles"],
        json!([
            "Evening skate",
            "Group class",
            "Hike",
            "House day",
            "Roller night",
            "Swim"
        ]),
        "{review}"
    );
    assert_eq!(
        reads["locations"],
        json!(["Pond Rink Example", "Rink, Main St; Lot B \\ north"])
    );
    assert_eq!(review["events"], 6);
    assert!(review["next"].as_array().unwrap().len() == 6, "{review}");
    assert!(!review.to_string().contains("SECRET-NEVER-SHARED"));
    assert_eq!(
        tool(&env, &key, "list_calendar_feeds", json!({})).await["feeds"],
        json!([])
    );
    // A wrong review id publishes nothing.
    let again = tool(
        &env,
        &key,
        "set_calendar_feed",
        json!({ "name": "mine", "title": "My plans", "sources": sources(), "confirm": "rev-nope" }),
    )
    .await;
    assert_eq!(again["review_id"], json!(id));

    let made = tool(
        &env,
        &key,
        "set_calendar_feed",
        json!({ "name": "mine", "title": "My plans", "sources": sources(), "confirm": id }),
    )
    .await;
    let url = made["url"]
        .as_str()
        .unwrap_or_else(|| panic!("{made}"))
        .to_owned();
    assert!(url.contains("/ical/cal-") && url.ends_with(".ics"), "{url}");

    // Anyone with the URL reads it: no token, no cookie.
    let (status, ty, ics) = fetch(&env, &url).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ty, "text/calendar; charset=utf-8");
    // CRLF lines, none longer than 75 octets.
    assert!(!ics.replace("\r\n", "").contains('\n'));
    for l in ics.split("\r\n") {
        assert!(l.len() <= 75, "{} octets: {l}", l.len());
    }
    let text = unfold(&ics);
    assert!(text.contains("X-WR-CALNAME:My plans\r\n"));
    assert!(text.contains("X-PUBLISHED-TTL:PT1H\r\n"));
    // Fields the sources don't map never go out, nor rows the where hides
    // or the window leaves out.
    for absent in [
        "NOTE-NEVER-SHARED",
        "SECRET-NEVER-SHARED",
        "Not going",
        "Attic",
    ] {
        assert!(!text.contains(absent), "{absent} in {text}");
    }
    assert_eq!(text.matches("BEGIN:VEVENT").count(), 6);

    // All day: a date, ending the next day.
    let house = events_with(&text, "SUMMARY:House day")[0];
    assert_eq!(line(house, "DTSTART"), "DTSTART;VALUE=DATE:20300309");
    assert_eq!(line(house, "DTEND"), "DTEND;VALUE=DATE:20300310");
    let hike = events_with(&text, "SUMMARY:Hike")[0];
    assert_eq!(line(hike, "DTSTART"), "DTSTART;VALUE=DATE:20300316");
    // "8:00 AM" to "09:00" in Denver before daylight saving: UTC-7.
    let class = events_with(&text, "SUMMARY:Group class")[0];
    assert_eq!(line(class, "DTSTART"), "DTSTART:20300308T150000Z");
    assert_eq!(line(class, "DTEND"), "DTEND:20300308T160000Z");
    // "3:00 PM" to "15:45" after it: UTC-6.
    let swim = events_with(&text, "SUMMARY:Swim")[0];
    assert_eq!(line(swim, "DTSTART"), "DTSTART:20300312T210000Z");
    assert_eq!(line(swim, "DTEND"), "DTEND:20300312T214500Z");
    // A datetime with its offset; no end: an hour. Text escaped.
    let night = events_with(&text, "SUMMARY:Roller night")[0];
    assert_eq!(line(night, "DTSTART"), "DTSTART:20300602T010000Z");
    assert_eq!(line(night, "DTEND"), "DTEND:20300602T020000Z");
    assert_eq!(
        line(night, "LOCATION"),
        "LOCATION:Rink\\, Main St\\; Lot B \\\\ north"
    );
    assert!(line(night, "DESCRIPTION").ends_with("at the door.\\nAll ages."));
    // A hand-added row: a datetime without an offset, in the pod's zone.
    let skating = events_with(&text, "SUMMARY:Evening skate")[0];
    assert_eq!(line(skating, "DTSTART"), "DTSTART:20301204T013000Z");
    assert_eq!(line(skating, "DTEND"), "DTEND:20301204T033000Z");
    assert_eq!(line(skating, "LOCATION"), "LOCATION:Pond Rink Example");
    assert_eq!(line(skating, "DESCRIPTION"), "DESCRIPTION:Bring gloves");
    let uid = line(skating, "UID").to_owned();
    assert!(uid.starts_with(&format!("UID:one_offs-{skate}@")), "{uid}");
    assert!(
        !uid.contains("plans"),
        "the pod's id stays out of the UID: {uid}"
    );

    // An edited row is the same event, renamed.
    let preview = tool(
        &env,
        &key,
        "edit_rows",
        json!({ "collection": "one_offs",
        "edits": [{ "op": "update", "id": skate, "values": { "title": "Evening skate party" } }] }),
    )
    .await;
    let done = tool(
        &env,
        &key,
        "edit_rows",
        json!({ "collection": "one_offs", "confirm": preview["confirm"] }),
    )
    .await;
    assert!(done.get("error").is_none(), "{done}");
    // Another one-off added by hand reaches the feed too.
    add(
        &env,
        &key,
        "one_offs",
        json!({ "title": "Gallery opening", "start": "2030-12-05T17:00", "location": "Gallery Example" }),
    )
    .await;
    let mut text = String::new();
    for _ in 0..100 {
        text = unfold(&fetch(&env, &url).await.2);
        if text.contains("Evening skate party") && text.contains("Gallery opening") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let renamed = events_with(&text, "SUMMARY:Evening skate party");
    assert_eq!(renamed.len(), 1, "{text}");
    assert_eq!(line(renamed[0], "UID"), uid);
    assert!(!text.contains("SUMMARY:Evening skate\r\n"));
    assert_eq!(text.matches(&uid).count(), 1);
    assert_eq!(text.matches("BEGIN:VEVENT").count(), 7);

    // Listed with its URL.
    let list = tool(&env, &key, "list_calendar_feeds", json!({})).await;
    assert_eq!(list["feeds"][0]["url"], json!(url), "{list}");
    assert_eq!(list["feeds"][0]["events"], 7);

    // Rotating: the old URL stops at once, the new one answers.
    let rotated = tool(
        &env,
        &key,
        "rotate_calendar_feed",
        json!({ "name": "mine" }),
    )
    .await;
    let new_url = rotated["url"]
        .as_str()
        .unwrap_or_else(|| panic!("{rotated}"))
        .to_owned();
    assert_ne!(new_url, url);
    assert_eq!(fetch(&env, &url).await.0, StatusCode::NOT_FOUND);
    let (status, _, rotated_ics) = fetch(&env, &new_url).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        unfold(&rotated_ics).contains(&uid),
        "UIDs survive a rotation"
    );

    // At most five feeds.
    for n in ["two", "three", "four", "five"] {
        let s = json!([{ "collection": "focus_days", "start": "date", "title": "{area}" }]);
        let r = tool(
            &env,
            &key,
            "set_calendar_feed",
            json!({ "name": n, "sources": s }),
        )
        .await;
        let r = tool(
            &env,
            &key,
            "set_calendar_feed",
            json!({ "name": n, "sources": s, "confirm": r["review_id"] }),
        )
        .await;
        assert!(r["url"].is_string(), "{r}");
    }
    let six = tool(
        &env,
        &key,
        "set_calendar_feed",
        json!({ "name": "six",
        "sources": [{ "collection": "focus_days", "start": "date", "title": "{area}" }] }),
    )
    .await;
    assert_eq!(six["error"], "too_many_feeds", "{six}");

    // Revoking: gone at once.
    let r = tool(
        &env,
        &key,
        "revoke_calendar_feed",
        json!({ "name": "mine" }),
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(fetch(&env, &new_url).await.0, StatusCode::NOT_FOUND);
    let names: Vec<Value> = tool(&env, &key, "list_calendar_feeds", json!({})).await["feeds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].clone())
        .collect();
    assert!(!names.contains(&json!("mine")), "{names:?}");
    // Not a feed token: not found, without asking the store.
    assert_eq!(
        fetch(&env, "/ical/pub-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ics")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

/// Feeds confirmed at once still stop at five, and every feed that went
/// live is one the pod lists (so it can be revoked): the feed list isn't
/// read by one confirm while another is saving it.
#[tokio::test]
async fn feeds_confirmed_at_once_keep_the_cap_and_stay_listed() {
    let (env, _files) = public_env().await;
    env.create("rush").await;
    let key = mcp_key(&env, "rush").await;
    define(
        &env,
        &key,
        "focus_days",
        json!([{ "name": "date", "type": "date" }, { "name": "area", "type": "text" }]),
    )
    .await;
    add(
        &env,
        &key,
        "focus_days",
        json!({ "date": "2026-03-02", "area": "Writing" }),
    )
    .await;
    let s = json!([{ "collection": "focus_days", "start": "date", "title": "{area}" }]);
    let mut asks = Vec::new();
    for n in 0..8 {
        let name = format!("feed_{n}");
        let r = tool(
            &env,
            &key,
            "set_calendar_feed",
            json!({ "name": name, "sources": s }),
        )
        .await;
        asks.push(json!({ "name": name, "sources": s, "confirm": r["review_id"] }));
    }
    let answers = futures_util::future::join_all(
        asks.iter()
            .map(|a| tool(&env, &key, "set_calendar_feed", a.clone())),
    )
    .await;
    let live: Vec<&Value> = answers.iter().filter(|r| r["url"].is_string()).collect();
    assert_eq!(live.len(), 5, "{answers:?}");
    let listed = tool(&env, &key, "list_calendar_feeds", json!({})).await;
    let urls: Vec<&Value> = listed["feeds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| &f["url"])
        .collect();
    assert_eq!(urls.len(), 5, "{listed}");
    for r in live {
        assert!(urls.contains(&&r["url"]), "{r} isn't listed: {listed}");
    }
}

/// Design `background-agent-security`, step 3: a collection a calendar feed
/// reads is written by no scoped credential, whatever its grant lists, so an
/// unattended job can't put text in front of subscribers.
#[tokio::test]
async fn a_scoped_credential_writes_no_rows_a_feed_reads() {
    let (env, _files) = public_env().await;
    env.create("plans").await;
    let key = mcp_key(&env, "plans").await;
    define(
        &env,
        &key,
        "focus_days",
        json!([{ "name": "date", "type": "date" }, { "name": "area", "type": "text" }]),
    )
    .await;
    add(
        &env,
        &key,
        "focus_days",
        json!({ "date": "2030-03-09", "area": "House" }),
    )
    .await;
    let scoped = {
        let host = env.host.clone();
        tokio::task::spawn_blocking(move || {
            let g = pod_core::grant::Grant::from_json(&json!({
                "tools": ["add_row"], "rows": ["focus_days"] }))
            .unwrap();
            host.issue_mcp_scoped("plans", "Anticipate", pod_core::mcp::Access::Edit, Some(&g))
                .unwrap()
        })
        .await
        .unwrap()
    };
    let row = json!({ "collection": "focus_days",
                      "values": { "date": "2030-03-10", "area": "Garden" } });
    // Before a feed reads it, the grant's own collection takes rows.
    let r = tool(&env, &scoped, "add_row", row.clone()).await;
    assert!(r.get("error").is_none(), "{r}");

    let sources = json!([{ "collection": "focus_days", "start": "date", "title": "{area} day" }]);
    let review = tool(
        &env,
        &key,
        "set_calendar_feed",
        json!({ "name": "mine", "sources": sources }),
    )
    .await;
    let set = tool(
        &env,
        &key,
        "set_calendar_feed",
        json!({ "name": "mine", "sources": sources, "confirm": review["review_id"] }),
    )
    .await;
    assert!(set.get("error").is_none(), "{set}");

    let r = tool(&env, &scoped, "add_row", row).await;
    assert_eq!(r["error"], "grant_refuses", "{r}");
    assert!(r.to_string().contains("others see"), "{r}");
}

/// Decision `mcp-access-levels`: a feed's url reads it from anywhere, so
/// only an outward connection is given it; read and edit ones list the feed
/// without it.
#[tokio::test]
async fn only_an_outward_connection_lists_a_feeds_url() {
    let (env, _files) = public_env().await;
    env.create("plans").await;
    let key = mcp_key(&env, "plans").await;
    define(
        &env,
        &key,
        "focus_days",
        json!([{ "name": "date", "type": "date" }, { "name": "area", "type": "text" }]),
    )
    .await;
    add(
        &env,
        &key,
        "focus_days",
        json!({ "date": "2030-03-09", "area": "House" }),
    )
    .await;
    let sources = json!([{ "collection": "focus_days", "start": "date", "title": "{area} day" }]);
    let review = tool(
        &env,
        &key,
        "set_calendar_feed",
        json!({ "name": "mine", "sources": sources }),
    )
    .await;
    let set = tool(
        &env,
        &key,
        "set_calendar_feed",
        json!({ "name": "mine", "sources": sources, "confirm": review["review_id"] }),
    )
    .await;
    let feed_url = set["url"]
        .as_str()
        .unwrap_or_else(|| panic!("{set}"))
        .to_owned();

    let outward = tool(&env, &key, "list_calendar_feeds", json!({})).await;
    assert_eq!(outward["feeds"][0]["url"], json!(feed_url), "{outward}");

    for access in [pod_core::mcp::Access::Read, pod_core::mcp::Access::Edit] {
        let below = {
            let host = env.host.clone();
            tokio::task::spawn_blocking(move || host.issue_mcp("plans", "test", access).unwrap())
                .await
                .unwrap()
        };
        let listed = tool(&env, &below, "list_calendar_feeds", json!({})).await;
        let feed = &listed["feeds"][0];
        assert_eq!(feed["name"], "mine", "{listed}");
        assert_eq!(feed["events"], 1, "{listed}");
        assert!(feed["sources"].is_array(), "{listed}");
        assert!(feed["updated_at"].is_number(), "{listed}");
        assert!(feed.get("url").is_none(), "{access:?}: {listed}");
        let token = feed_url
            .rsplit('/')
            .next()
            .unwrap()
            .trim_end_matches(".ics");
        assert!(!listed.to_string().contains(token), "{access:?}: {listed}");
    }
}
