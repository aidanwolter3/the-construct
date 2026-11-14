//! `core://Text` `format` (feature `date_format`, decision
//! `text-dates-and-counts`), as the shared UI draws it (`TextFormat.kt`):
//! bound `YYYY-MM-DD` values as dates the viewer reads, against the
//! device's own clock and time zone.

use protocol::DateFormat;

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 (H. Hinnant's `days_from_civil`).
fn epoch_day(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `(year, month, day)` of an epoch day.
fn civil(day: i64) -> (i64, i64, i64) {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// The epoch day of exactly a `YYYY-MM-DD` date (a real calendar day).
pub fn iso_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = &s[r];
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    Some(epoch_day(y, m, d))
}

/// `day` drawn as `format` seen on `today` (epoch days): `Relative` is
/// "today", "tomorrow", "yesterday", "in N days" or "N days ago" up to six
/// days, else as `Short`: "Nov 5", with the year when not today's year.
pub fn format_date(day: i64, today: i64, format: DateFormat) -> String {
    if format == DateFormat::Relative {
        match day - today {
            0 => return "today".into(),
            1 => return "tomorrow".into(),
            -1 => return "yesterday".into(),
            d @ 2..=6 => return format!("in {d} days"),
            d @ -6..=-2 => return format!("{} days ago", -d),
            _ => {}
        }
    }
    let (y, m, d) = civil(day);
    let short = format!("{} {d}", MONTHS[(m - 1) as usize]);
    if y == civil(today).0 {
        short
    } else {
        format!("{short}, {y}")
    }
}

/// Seconds since 1970-01-01 00:00 on the wall clock of `zone` (this
/// device's zone when `None`; UTC when that can't be read).
pub fn local_seconds(zone: Option<&jiff::tz::TimeZone>) -> i64 {
    let now = match zone {
        Some(z) => jiff::Timestamp::now().to_zoned(z.clone()),
        None => jiff::Zoned::now(),
    };
    wall_seconds(now.datetime())
}

fn wall_seconds(dt: jiff::civil::DateTime) -> i64 {
    epoch_day(
        i64::from(dt.year()),
        i64::from(dt.month()),
        i64::from(dt.day()),
    ) * 86_400
        + i64::from(dt.hour()) * 3_600
        + i64::from(dt.minute()) * 60
        + i64::from(dt.second())
}

/// A bound moment (`core://Conditional` `date`): its local epoch day, and
/// its minute of that day when it has a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moment {
    pub day: i64,
    pub minute: Option<i64>,
}

/// Reads `value` as a moment on the wall clock of `zone`: `YYYY-MM-DD`
/// (at `time`, when that reads as a time of day), a local
/// `YYYY-MM-DDTHH:MM[:SS]`, or an instant with an offset or `Z`.
pub fn moment(value: &str, time: &str, zone: Option<&jiff::tz::TimeZone>) -> Option<Moment> {
    let value = value.trim();
    if let Some(day) = iso_date(value) {
        return Some(Moment {
            day,
            minute: time_of_day(time),
        });
    }
    let day = iso_date(value.get(..10)?)?;
    let rest = value.get(10..)?;
    let local = rest
        .strip_prefix(['T', ' '])
        .filter(|t| {
            t.len() >= 5
                && t[5..]
                    .bytes()
                    .all(|c| c.is_ascii_digit() || b":.".contains(&c))
        })
        .and_then(|t| hh_mm(&t[..5]));
    if let Some(minute) = local {
        return Some(Moment {
            day,
            minute: Some(minute),
        });
    }
    let at: jiff::Timestamp = value.parse().ok()?;
    let dt = match zone {
        Some(z) => at.to_zoned(z.clone()),
        None => at.to_zoned(jiff::tz::TimeZone::system()),
    }
    .datetime();
    let secs = wall_seconds(dt);
    Some(Moment {
        day: secs.div_euclid(86_400),
        minute: Some(secs.rem_euclid(86_400) / 60),
    })
}

/// `HH:MM` (24-hour) as minutes of the day.
fn hh_mm(s: &str) -> Option<i64> {
    let (h, m) = s.split_once(':')?;
    let (h, m): (i64, i64) = (h.parse().ok()?, m.parse().ok()?);
    ((0..24).contains(&h) && (0..60).contains(&m) && s.len() == 5).then_some(h * 60 + m)
}

/// A time of day as people write it: `15:45`, `9:00`, `9:00 AM`, `3:45pm`,
/// `9 AM` (seconds are dropped). Anything else is no time.
pub fn time_of_day(s: &str) -> Option<i64> {
    let s = s.trim().to_ascii_lowercase();
    let (clock, pm) = match s.strip_suffix("am").or_else(|| s.strip_suffix("a.m.")) {
        Some(c) => (c.trim_end(), Some(false)),
        None => match s.strip_suffix("pm").or_else(|| s.strip_suffix("p.m.")) {
            Some(c) => (c.trim_end(), Some(true)),
            None => (s.as_str(), None),
        },
    };
    let mut parts = clock.split(':');
    let h = parts.next()?;
    let m = parts.next().unwrap_or(if pm.is_some() { "00" } else { "" });
    let s_part = parts.next();
    let digits = |p: &str, n: std::ops::RangeInclusive<usize>| {
        n.contains(&p.len()) && p.bytes().all(|c| c.is_ascii_digit())
    };
    if !digits(h, 1..=2) || !digits(m, 2..=2) || s_part.is_some_and(|x| !digits(x, 2..=2)) {
        return None;
    }
    if parts.next().is_some() {
        return None;
    }
    let (mut h, m): (i64, i64) = (h.parse().ok()?, m.parse().ok()?);
    match pm {
        Some(pm) => {
            if !(1..=12).contains(&h) {
                return None;
            }
            h = h % 12 + if pm { 12 } else { 0 };
        }
        None if h > 23 => return None,
        None => {}
    }
    (m < 60).then_some(h * 60 + m)
}

/// Whether `at` passes `test` at `now` (local minutes since the epoch), and
/// the next minute after `now` at which the answer changes, if it ever
/// does.
pub fn passes(test: protocol::DateTest, at: Moment, now: i64) -> (bool, Option<i64>) {
    use protocol::DateTest;
    let start = at.day * 1_440;
    let shows = |from: i64, until: i64| {
        let next = [from, until].into_iter().find(|&b| b > now && b < i64::MAX);
        (from <= now && now < until, next)
    };
    match test {
        DateTest::Today => shows(start, start + 1_440),
        DateTest::Past => shows(at.minute.map_or(start + 1_440, |m| start + m), i64::MAX),
        DateTest::Future => shows(i64::MIN, at.minute.map_or(start, |m| start + m)),
    }
}

/// What frames read the time from.
#[derive(Debug, Clone, Default)]
pub struct Clock {
    /// A fixed wall-clock time, in seconds since the epoch (tests, the
    /// simulator's `--now`); `None`: the live clock.
    pub fixed: Option<i64>,
    /// The zone the live clock reads in (a pod's preview reads in the pod's
    /// zone); `None`: this device's.
    pub zone: Option<jiff::tz::TimeZone>,
}

impl Clock {
    /// Seconds since 1970-01-01 00:00 on this clock's wall.
    pub fn seconds(&self) -> i64 {
        self.fixed
            .unwrap_or_else(|| local_seconds(self.zone.as_ref()))
    }

    /// Minutes since 1970-01-01 00:00 on this clock's wall.
    pub fn minute(&self) -> i64 {
        self.seconds().div_euclid(60)
    }

    /// How long until local minute `minute` begins; `None` for a fixed
    /// clock (it never gets there).
    pub fn until(&self, minute: i64) -> Option<std::time::Duration> {
        if self.fixed.is_some() {
            return None;
        }
        let secs = (minute * 60 - self.seconds()).max(0);
        Some(std::time::Duration::from_secs(secs.unsigned_abs()))
    }
}

/// A wall-clock time written `YYYY-MM-DDTHH:MM` (the simulator's `--now`),
/// in seconds since the epoch.
pub fn wall_time(s: &str) -> Option<i64> {
    let m = moment(s, "", None).filter(|_| s.trim().len() == 16)?;
    Some((m.day * 1_440 + m.minute?) * 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_read_as_the_shared_ui_draws_them() {
        let today = iso_date("2026-11-07").unwrap();
        let f = |s: &str, fmt| format_date(iso_date(s).unwrap(), today, fmt);
        assert_eq!(f("2026-11-07", DateFormat::Relative), "today");
        assert_eq!(f("2026-11-08", DateFormat::Relative), "tomorrow");
        assert_eq!(f("2026-11-06", DateFormat::Relative), "yesterday");
        assert_eq!(f("2026-11-10", DateFormat::Relative), "in 3 days");
        assert_eq!(f("2026-11-01", DateFormat::Relative), "6 days ago");
        assert_eq!(f("2026-11-14", DateFormat::Relative), "Nov 14");
        assert_eq!(f("2027-01-05", DateFormat::Short), "Jan 5, 2027");
        assert_eq!(f("2026-11-07", DateFormat::Short), "Nov 7");
        assert_eq!(iso_date("2026-02-29"), None);
        assert_eq!(iso_date("2024-02-29").map(civil), Some((2024, 2, 29)));
        assert_eq!(iso_date("1970-01-01"), Some(0));
        assert_eq!(iso_date("not a date"), None);
        assert_eq!(iso_date("2026-11-07T10:00"), None);
    }

    #[test]
    fn times_read_as_people_write_them() {
        for (s, m) in [
            ("15:45", 945),
            ("09:00", 540),
            ("9:00", 540),
            ("9:00 AM", 540),
            ("3:45pm", 945),
            ("3:45 p.m.", 945),
            ("9 AM", 540),
            ("12:30 AM", 30),
            ("12:00 PM", 720),
            ("08:15:30", 495),
            (" 0:00 ", 0),
        ] {
            assert_eq!(time_of_day(s), Some(m), "{s}");
        }
        for s in [
            "",
            "Bonus",
            "25:00",
            "9",
            "13:00 PM",
            "9:5",
            "9:00:00:00",
            "0 AM",
        ] {
            assert_eq!(time_of_day(s), None, "{s}");
        }
    }

    #[test]
    fn moments_read_dates_local_times_and_instants() {
        let day = iso_date("2026-11-16").unwrap();
        let at = |minute| Some(Moment { day, minute });
        assert_eq!(moment("2026-11-16", "", None), at(None));
        assert_eq!(moment("2026-11-16", "Bonus", None), at(None));
        assert_eq!(moment("2026-11-16", "3:45 PM", None), at(Some(945)));
        assert_eq!(moment("2026-11-16T09:00", "", None), at(Some(540)));
        assert_eq!(moment("2026-11-16 09:00:00", "15:00", None), at(Some(540)));
        let denver = jiff::tz::TimeZone::get("America/Denver").unwrap();
        assert_eq!(
            moment("2026-11-16T16:00:00Z", "", Some(&denver)),
            at(Some(540))
        );
        assert_eq!(
            moment("2026-11-17T03:30:00+00:00", "", Some(&denver)),
            at(Some(20 * 60 + 30))
        );
        assert_eq!(moment("next Monday", "", None), None);
        assert_eq!(moment("", "9:00", None), None);
    }

    #[test]
    fn tests_answer_now_and_when_they_change() {
        use protocol::DateTest::{Future, Past, Today};
        let day = iso_date("2026-11-16").unwrap();
        let start = day * 1_440;
        let bare = Moment { day, minute: None };
        let nine = Moment {
            day,
            minute: Some(540),
        };
        // The day before, at noon.
        let before = start - 720;
        assert_eq!(passes(Past, bare, before), (false, Some(start + 1_440)));
        assert_eq!(passes(Today, bare, before), (false, Some(start)));
        assert_eq!(passes(Future, bare, before), (true, Some(start)));
        assert_eq!(passes(Past, nine, before), (false, Some(start + 540)));
        assert_eq!(passes(Future, nine, before), (true, Some(start + 540)));
        // That day, 08:59 and 09:00.
        assert_eq!(passes(Past, nine, start + 539), (false, Some(start + 540)));
        assert_eq!(passes(Past, nine, start + 540), (true, None));
        assert_eq!(passes(Future, nine, start + 540), (false, None));
        assert_eq!(
            passes(Today, nine, start + 540),
            (true, Some(start + 1_440))
        );
        assert_eq!(
            passes(Past, bare, start + 540),
            (false, Some(start + 1_440))
        );
        assert_eq!(passes(Future, bare, start + 540), (false, None));
        // The day after.
        assert_eq!(passes(Past, bare, start + 1_440), (true, None));
        assert_eq!(passes(Today, bare, start + 1_440), (false, None));
    }

    #[test]
    fn a_fixed_clock_reads_its_time_and_never_wakes() {
        let at = wall_time("2026-11-16T09:30").unwrap();
        let clock = Clock {
            fixed: Some(at),
            zone: None,
        };
        assert_eq!(
            clock.minute(),
            iso_date("2026-11-16").unwrap() * 1_440 + 570
        );
        assert_eq!(clock.until(clock.minute() + 5), None);
        assert_eq!(wall_time("2026-11-16"), None);
        assert_eq!(wall_time("2026-11-16T09:30:00"), None);
    }
}
