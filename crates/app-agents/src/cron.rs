//! Five-field cron expressions read in a time zone (decision
//! `agents-cap-v3`): `minute hour day-of-month month day-of-week`, each `*`,
//! a number, a range `a-b`, a step `*/n` or `a-b/n`, or a list of those;
//! months and days may be named (`jan`, `mon`), and day 7 is Sunday too.
//! When both day fields are restricted, a day matching either one counts
//! (as in Vixie cron).
//!
//! An occurrence is named by its local minute (`2026-11-15T03:52`), the
//! schedule's work-item occurrence. Daylight saving: a local time that
//! doesn't exist (the hour clocks skip) runs at the first instant after the
//! gap, and one that happens twice (the hour clocks repeat) runs once, at
//! the first of the two. Built on the zone database the pod already ships
//! (`pod_core::tz`), so it needs no cron crate.

use pod_core::tz::jiff::{
    Timestamp,
    civil::{Date, DateTime},
    tz::TimeZone,
};

/// A parsed expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minutes: Vec<i8>,
    hours: Vec<i8>,
    days: Vec<i8>,
    months: Vec<i8>,
    weekdays: Vec<i8>,
    /// Day of month restricted (not `*`).
    days_set: bool,
    /// Day of week restricted (not `*`).
    weekdays_set: bool,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

fn value(s: &str, names: &[&str], base: i8) -> Option<i8> {
    if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(s)) {
        return Some(i as i8 + base);
    }
    s.parse().ok()
}

/// One field: the sorted values it allows within `lo..=hi`, and whether it
/// is restricted.
fn field(s: &str, lo: i8, hi: i8, names: &[&str], base: i8) -> Result<(Vec<i8>, bool), String> {
    let mut out = Vec::new();
    let mut restricted = true;
    for part in s.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, st)) => (
                r,
                st.parse::<i8>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| format!("'{part}' has a bad step"))?,
            ),
            None => (part, 1),
        };
        let (a, b) = if range == "*" {
            if s == "*" {
                restricted = false;
            }
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            let a = value(a, names, base).ok_or_else(|| format!("'{part}' is not a range"))?;
            let b = value(b, names, base).ok_or_else(|| format!("'{part}' is not a range"))?;
            (a, b)
        } else {
            let a = value(range, names, base).ok_or_else(|| format!("'{part}' is not a number"))?;
            (a, if step > 1 { hi } else { a })
        };
        if a < lo || b > hi || a > b {
            return Err(format!("'{part}' is outside {lo}-{hi}"));
        }
        let mut v = a;
        while v <= b {
            out.push(v);
            v = match v.checked_add(step) {
                Some(n) => n,
                None => break,
            };
        }
    }
    out.sort_unstable();
    out.dedup();
    if out.is_empty() {
        return Err(format!("'{s}' allows nothing"));
    }
    Ok((out, restricted))
}

impl Cron {
    /// Parse `expr` (five fields).
    pub fn parse(expr: &str) -> Result<Self, String> {
        let parts: Vec<&str> = expr.split_whitespace().collect();
        let [mi, h, d, mo, wd] = parts.as_slice() else {
            return Err(
                "a cron has five fields: minute hour day-of-month month day-of-week, e.g. 0 3 * * *"
                    .into(),
            );
        };
        let (minutes, _) = field(mi, 0, 59, &[], 0)?;
        let (hours, _) = field(h, 0, 23, &[], 0)?;
        let (days, days_set) = field(d, 1, 31, &[], 0)?;
        let (months, _) = field(mo, 1, 12, &MONTHS, 1)?;
        let (mut weekdays, weekdays_set) = field(wd, 0, 7, &DAYS, 0)?;
        if weekdays.contains(&7) {
            weekdays.retain(|d| *d != 7);
            if !weekdays.contains(&0) {
                weekdays.insert(0, 0);
            }
        }
        Ok(Self {
            minutes,
            hours,
            days,
            months,
            weekdays,
            days_set,
            weekdays_set,
        })
    }

    fn day_matches(&self, d: Date) -> bool {
        if !self.months.contains(&d.month()) {
            return false;
        }
        let dom = self.days.contains(&d.day());
        let dow = self.weekdays.contains(&d.weekday().to_sunday_zero_offset());
        match (self.days_set, self.weekdays_set) {
            (true, true) => dom || dow,
            (true, false) => dom,
            (false, true) => dow,
            (false, false) => true,
        }
    }

    /// The occurrences after `after` up to and including `until`, oldest
    /// first, at most `limit` (the newest kept when there are more): each
    /// its local minute's name and its instant.
    pub fn between(
        &self,
        tz: &TimeZone,
        after: Timestamp,
        until: Timestamp,
        limit: usize,
    ) -> Vec<(String, Timestamp)> {
        let mut out = Vec::new();
        if until <= after || limit == 0 {
            return out;
        }
        // From the day before (a skipped hour moves a time forward) to the
        // day after, in local days.
        let Ok(mut day) = after.to_zoned(tz.clone()).date().yesterday() else {
            return out;
        };
        let Ok(last) = until.to_zoned(tz.clone()).date().tomorrow() else {
            return out;
        };
        while day <= last {
            if self.day_matches(day) {
                for &h in &self.hours {
                    for &m in &self.minutes {
                        let local = day.at(h, m, 0, 0);
                        let Some(at) = instant(tz, local) else {
                            continue;
                        };
                        if at > after && at <= until {
                            out.push((name(local), at));
                        }
                    }
                }
            }
            day = match day.tomorrow() {
                Ok(d) => d,
                Err(_) => break,
            };
        }
        out.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
        if out.len() > limit {
            out.drain(..out.len() - limit);
        }
        out
    }

    /// The first occurrence after `after` (within about a year and a day).
    pub fn next_after(&self, tz: &TimeZone, after: Timestamp) -> Option<(String, Timestamp)> {
        let mut from = after;
        // A month at a time, so a daily cron costs one short look.
        for _ in 0..13 {
            let to = from
                .checked_add(pod_core::tz::jiff::SignedDuration::from_hours(24 * 31))
                .ok()?;
            if let Some(first) = self.between(tz, from, to, usize::MAX).into_iter().next() {
                return Some(first);
            }
            from = to;
        }
        None
    }
}

/// The instant a local time names: after the gap for one that doesn't
/// exist, the first of two for one that happens twice.
fn instant(tz: &TimeZone, local: DateTime) -> Option<Timestamp> {
    tz.to_ambiguous_zoned(local)
        .compatible()
        .ok()
        .map(|z| z.timestamp())
}

/// A local minute's name, e.g. `2026-11-15T03:52`.
pub fn name(local: DateTime) -> String {
    local.strftime("%Y-%m-%dT%H:%M").to_string()
}

/// The local minute of instant `at` in `tz`.
pub fn minute_of(tz: &TimeZone, at: Timestamp) -> String {
    name(at.to_zoned(tz.clone()).datetime())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn tz(n: &str) -> TimeZone {
        TimeZone::get(n).unwrap()
    }

    #[test]
    fn expressions_parse() {
        assert!(Cron::parse("0 3 * * *").is_ok());
        assert!(Cron::parse("*/15 9-17 * * mon-fri").is_ok());
        assert!(Cron::parse("0 0 1,15 jan,jul 7").is_ok());
        for bad in [
            "",
            "0 3 * *",
            "60 * * * *",
            "* 24 * * *",
            "5-1 * * * *",
            "*/0 * * * *",
            "x * * * *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn occurrences_read_in_the_zone() {
        let c = Cron::parse("52 3 * * *").unwrap();
        let denver = tz("America/Denver");
        let got = c.between(
            &denver,
            ts("2026-11-14T12:00:00Z"),
            ts("2026-11-16T12:00:00Z"),
            10,
        );
        let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["2026-11-15T03:52", "2026-11-16T03:52"]);
        assert_eq!(got[0].1, ts("2026-11-15T10:52:00Z"));
        let next = c.next_after(&denver, ts("2026-11-15T10:52:00Z")).unwrap();
        assert_eq!(next.0, "2026-11-16T03:52");
        assert_eq!(
            minute_of(&denver, ts("2026-11-15T10:52:30Z")),
            "2026-11-15T03:52"
        );
    }

    #[test]
    fn daylight_saving_skips_and_repeats_run_once() {
        let ny = tz("America/New_York");
        // 2026-03-08: 02:00 → 03:00. 02:30 doesn't exist: it runs at 03:00.
        let c = Cron::parse("30 2 * * *").unwrap();
        let got = c.between(
            &ny,
            ts("2026-03-07T12:00:00Z"),
            ts("2026-03-08T12:00:00Z"),
            10,
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "2026-03-08T02:30");
        assert_eq!(got[0].1, ts("2026-03-08T07:30:00Z"));
        // 2026-11-01: 02:00 → 01:00. 01:30 happens twice: it runs once.
        let c = Cron::parse("30 1 * * *").unwrap();
        let got = c.between(
            &ny,
            ts("2026-10-31T12:00:00Z"),
            ts("2026-11-01T12:00:00Z"),
            10,
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, ts("2026-11-01T05:30:00Z"), "the first of the two");
        // Every 30 minutes across the repeated hour: each local minute once.
        let c = Cron::parse("*/30 * * * *").unwrap();
        let got = c.between(
            &ny,
            ts("2026-11-01T04:00:00Z"),
            ts("2026-11-01T07:00:00Z"),
            100,
        );
        let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "2026-11-01T00:30",
                "2026-11-01T01:00",
                "2026-11-01T01:30",
                "2026-11-01T02:00"
            ]
        );
    }

    #[test]
    fn both_day_fields_match_either() {
        // The 13th, or any Friday.
        let c = Cron::parse("0 9 13 * fri").unwrap();
        let utc = TimeZone::UTC;
        let got = c.between(
            &utc,
            ts("2026-11-01T00:00:00Z"),
            ts("2026-11-15T00:00:00Z"),
            10,
        );
        let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["2026-11-06T09:00", "2026-11-13T09:00"]);
        assert_eq!(
            c.between(
                &utc,
                ts("2026-11-01T00:00:00Z"),
                ts("2026-12-31T00:00:00Z"),
                2
            )
            .len(),
            2,
            "capped at the newest"
        );
    }
}
