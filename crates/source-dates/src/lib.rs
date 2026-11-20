//! The `dates` source: the days around today, in the pod's time zone
//! (decision `element-roles-and-reuse`).
//!
//! A layout that lists days (an agenda, a week strip, a day picker) needs
//! rows to list. Pods kept collections of dates for that, filled by hand or
//! by a schedule, which exist only to drive a screen. This source answers
//! them from the clock: one feed, `days`, whose queries are the usual
//! windows (today, this week, the next 7, 14 and 30 days, the last 7, this
//! month), each day an entity `dates://day/<YYYY-MM-DD>`.
//!
//! What is today is still the device's to say: test a day with
//! `core://Conditional` `date` on `$item.date`, which follows the device's
//! clock to the minute. The pod tells its devices to load the feed again
//! when its own day turns.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use pod_core::source::{EntityDoc, FeedSpec, SourceManifest};
use pod_core::tz::jiff::civil::{Date, Weekday};
use pod_core::tz::jiff::{self, ToSpan, tz::TimeZone};
use pod_core::{BlueprintInfo, BoxFuture, Hydration, Pod, RequestCtx, Source};
use protocol::{DataBundle, EntityRecord};
use serde_json::{Value, json};

pub const NAME: &str = "dates";
pub const FEED: &str = "days";

pub const TODAY: &str = "dates:today";
pub const THIS_WEEK: &str = "dates:this_week";
pub const NEXT_WEEK: &str = "dates:next_week";
pub const NEXT_7: &str = "dates:next_7";
pub const NEXT_14: &str = "dates:next_14";
pub const NEXT_30: &str = "dates:next_30";
pub const LAST_7: &str = "dates:last_7";
pub const MONTH: &str = "dates:month";

pub const MANIFEST: SourceManifest = SourceManifest {
    entity_schemes: &[NAME],
    feeds: &[FeedSpec {
        name: FEED,
        query_keys: &[
            TODAY, THIS_WEEK, NEXT_WEEK, NEXT_7, NEXT_14, NEXT_30, LAST_7, MONTH,
        ],
        doc: "The days around today in the pod's time zone (set_time_zone), one entity per \
              day: dates:today, dates:this_week and dates:next_week (Monday to Sunday), \
              dates:next_7, dates:next_14 and dates:next_30 (from today on), dates:last_7 (up \
              to today) and dates:month (this calendar month). List one to draw an agenda, a \
              week strip or a day picker; join other rows by date with a list inside the \
              day's template (where: their date field equals $item.date). Use it instead of \
              a collection of dates.",
    }],
    entities: &[EntityDoc {
        uri: "dates://day/{date}",
        doc: "One day.",
        fields: &[
            ("date", "YYYY-MM-DD"),
            ("weekday", "Mon"),
            ("weekday_long", "Monday"),
            ("day", "the day of the month, e.g. 5"),
            ("month", "Nov"),
            ("month_long", "November"),
            ("year", "e.g. 2026"),
            ("week", "the Monday of its week, YYYY-MM-DD"),
            ("weekend", "true on Saturday and Sunday"),
            (
                "offset",
                "days from the pod's today: 0 today, 1 tomorrow, -1 yesterday",
            ),
            ("label", "Today, Tomorrow, Yesterday, else Fri Nov 20"),
        ],
    }],
    ..SourceManifest::new("Dates")
};

pub struct DatesAdapter;

pub fn adapter() -> Arc<dyn Source> {
    Arc::new(DatesAdapter)
}

impl Source for DatesAdapter {
    fn name(&self) -> &'static str {
        NAME
    }

    fn blueprints(&self) -> Vec<BlueprintInfo> {
        Vec::new()
    }

    fn layout_files(&self) -> &'static [pod_core::layouts::LayoutSource] {
        &[]
    }

    fn manifest(&self) -> SourceManifest {
        MANIFEST
    }

    fn feed<'a>(
        &'a self,
        pod: &'a Pod,
        feed: &'a str,
        _req: &'a RequestCtx,
    ) -> BoxFuture<'a, Hydration> {
        Box::pin(async move {
            if feed != FEED {
                return pod_core::unknown_feed(NAME, feed);
            }
            let tz = pod
                .blocking(|s| pod_core::tz::time_zone(s))
                .await
                .unwrap_or(TimeZone::UTC);
            turn_with_the_day(pod);
            Hydration::Data(bundle(today(&tz, pod_core::util::now_secs())))
        })
    }
}

/// The date it is in `tz` at `now` (Unix seconds).
pub fn today(tz: &TimeZone, now: i64) -> Date {
    jiff::Timestamp::from_second(now)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .to_zoned(tz.clone())
        .date()
}

fn uri(d: Date) -> String {
    format!("dates://day/{d}")
}

fn monday(d: Date) -> Date {
    d.saturating_sub((d.weekday().to_monday_zero_offset() as i64).days())
}

fn day(d: Date, today: Date) -> EntityRecord {
    let offset = (d - today).get_days();
    let label = match offset {
        0 => "Today".to_owned(),
        1 => "Tomorrow".to_owned(),
        -1 => "Yesterday".to_owned(),
        _ => d.strftime("%a %b %-d").to_string(),
    };
    let v = json!({
        "id": d.to_string(),
        "uri": uri(d),
        "date": d.to_string(),
        "weekday": d.strftime("%a").to_string(),
        "weekday_long": d.strftime("%A").to_string(),
        "day": d.day().to_string(),
        "month": d.strftime("%b").to_string(),
        "month_long": d.strftime("%B").to_string(),
        "year": d.year().to_string(),
        "week": monday(d).to_string(),
        "weekend": matches!(d.weekday(), Weekday::Saturday | Weekday::Sunday),
        "offset": offset,
        "label": label,
    });
    match v {
        Value::Object(m) => EntityRecord(m),
        _ => EntityRecord::new(),
    }
}

/// The feed for the day `today`: every query's days, each day once.
pub fn bundle(today: Date) -> DataBundle {
    let mut b = DataBundle::default();
    let mut put = |key: &str, from: Date, days: i64| {
        let mut uris = Vec::new();
        for i in 0..days {
            let d = from.saturating_add(i.days());
            b.entities.entry(uri(d)).or_insert_with(|| day(d, today));
            uris.push(uri(d));
        }
        b.queries.insert(key.to_owned(), uris);
    };
    let week = monday(today);
    put(TODAY, today, 1);
    put(THIS_WEEK, week, 7);
    put(NEXT_WEEK, week.saturating_add(7.days()), 7);
    put(NEXT_7, today, 7);
    put(NEXT_14, today, 14);
    put(NEXT_30, today, 30);
    put(LAST_7, today.saturating_sub(6.days()), 7);
    put(
        MONTH,
        today.first_of_month(),
        i64::from(today.days_in_month()),
    );
    b
}

/// The pods whose devices are told when the day turns.
fn watched() -> &'static Mutex<HashSet<usize>> {
    static WATCHED: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();
    WATCHED.get_or_init(Default::default)
}

/// Once a pod's layouts read the feed, tell its devices to load it again
/// at each midnight of the pod's zone, for as long as the pod is open.
fn turn_with_the_day(pod: &Pod) {
    let key = Arc::as_ptr(pod) as usize;
    if !watched()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key)
    {
        return;
    }
    let weak = Arc::downgrade(pod);
    tokio::spawn(async move {
        loop {
            let Some(pod) = weak.upgrade() else {
                break;
            };
            let tz = pod
                .blocking(|s| pod_core::tz::time_zone(s))
                .await
                .unwrap_or(TimeZone::UTC);
            let wait = until_midnight(&tz, pod_core::util::now_secs());
            drop(pod);
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            match weak.upgrade() {
                Some(pod) => pod.publish_data_all(NAME),
                None => break,
            }
        }
        watched()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
    });
}

/// Seconds from `now` to just after the next midnight in `tz` (a zone's
/// change of clocks can move it; an hour either way, at least a minute).
pub fn until_midnight(tz: &TimeZone, now: i64) -> u64 {
    let next = today(tz, now).saturating_add(1.days());
    let at = next
        .at(0, 0, 5, 0)
        .to_zoned(tz.clone())
        .map(|z| z.timestamp().as_second())
        .unwrap_or(now + 86_400);
    (at - now).clamp(60, 90_000) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i16, m: i8, d: i8) -> Date {
        Date::new(y, m, d).unwrap()
    }

    #[test]
    fn the_windows_are_around_today() {
        // Friday, November 20, 2026.
        let b = bundle(date(2026, 11, 20));
        assert_eq!(b.queries[TODAY], ["dates://day/2026-11-20"]);
        let week = &b.queries[THIS_WEEK];
        assert_eq!(week.len(), 7);
        assert_eq!(week[0], "dates://day/2026-11-16");
        assert_eq!(week[6], "dates://day/2026-11-22");
        assert_eq!(b.queries[NEXT_WEEK][0], "dates://day/2026-11-23");
        assert_eq!(b.queries[NEXT_7][6], "dates://day/2026-11-26");
        assert_eq!(b.queries[NEXT_14].len(), 14);
        // The next 30 days run into December.
        assert_eq!(b.queries[NEXT_30][29], "dates://day/2026-12-19");
        assert_eq!(b.queries[LAST_7][0], "dates://day/2026-11-14");
        assert_eq!(b.queries[LAST_7][6], "dates://day/2026-11-20");
        assert_eq!(b.queries[MONTH].len(), 30);
        assert_eq!(b.queries[MONTH][0], "dates://day/2026-11-01");

        let today = &b.entities["dates://day/2026-11-20"];
        assert_eq!(today.get("weekday").unwrap(), "Fri");
        assert_eq!(today.get("weekday_long").unwrap(), "Friday");
        assert_eq!(today.get("day").unwrap(), "20");
        assert_eq!(today.get("month").unwrap(), "Nov");
        assert_eq!(today.get("week").unwrap(), "2026-11-16");
        assert_eq!(today.get("weekend").unwrap(), false);
        assert_eq!(today.get("offset").unwrap(), 0);
        assert_eq!(today.get("label").unwrap(), "Today");
        let sat = &b.entities["dates://day/2026-11-21"];
        assert_eq!(sat.get("label").unwrap(), "Tomorrow");
        assert_eq!(sat.get("weekend").unwrap(), true);
        assert_eq!(
            b.entities["dates://day/2026-11-19"].get("label").unwrap(),
            "Yesterday"
        );
        assert_eq!(
            b.entities["dates://day/2026-12-05"].get("label").unwrap(),
            "Sat Dec 5"
        );
        assert_eq!(
            b.entities["dates://day/2026-11-14"].get("offset").unwrap(),
            -6
        );
    }

    #[test]
    fn today_and_midnight_follow_the_zone() {
        // 2026-11-21 03:30 UTC is still the 20th in Denver (20:30).
        let now = jiff::civil::date(2026, 11, 21)
            .at(3, 30, 0, 0)
            .to_zoned(TimeZone::UTC)
            .unwrap()
            .timestamp()
            .as_second();
        let denver = TimeZone::get("America/Denver").unwrap();
        assert_eq!(today(&TimeZone::UTC, now), date(2026, 11, 21));
        assert_eq!(today(&denver, now), date(2026, 11, 20));
        // Midnight in Denver is 3 h 30 min away, and a few seconds past it.
        assert_eq!(until_midnight(&denver, now), 3 * 3600 + 30 * 60 + 5);
        assert_eq!(until_midnight(&TimeZone::UTC, now), 20 * 3600 + 30 * 60 + 5);
    }
}
