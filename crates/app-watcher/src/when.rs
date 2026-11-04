//! Event dates: what a source wrote, kept as its wall clock, plus its UTC
//! offset when the source gave one (or named a zone we could resolve).
//!
//! Layouts can't format dates, so the pod writes the display text
//! (`Fri Nov 6 · 7:30 PM`) from the source's own wall clock: an event in
//! Boulder reads in Boulder time, whatever zone the pod runs in. A time
//! with an offset (`…T19:30-07:00`, `…Z`) is exact; one without is placed
//! in the target's [`Zone`] when it has one (a named zone follows daylight
//! saving; a fixed offset doesn't), else read as UTC (only "is it past"
//! and date windows use it, with a day of slack).

use pod_core::tz::jiff::Timestamp;
use pod_core::tz::jiff::tz::{AmbiguousOffset, TimeZone};

/// A target's zone: an IANA zone (`America/Denver`, with its daylight
/// saving rules; the database is `pod_core::tz`'s, built in) or a fixed
/// offset in minutes east of UTC (targets made before zones had names).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Zone {
    Named(TimeZone),
    Fixed(i32),
}

impl Zone {
    /// Minutes east of UTC at the instant `utc_secs`.
    pub fn offset_at(&self, utc_secs: i64) -> i32 {
        match self {
            Zone::Fixed(o) => *o,
            Zone::Named(tz) => Timestamp::from_second(utc_secs)
                .map(|ts| tz.to_offset(ts).seconds() / 60)
                .unwrap_or(0),
        }
    }

    /// Minutes east of UTC for a wall clock (`wall_secs`: the wall clock
    /// read as UTC) in this zone. A time that happens twice (the hour
    /// daylight saving ends) takes the first; one that is skipped takes
    /// the offset from before the change.
    pub fn offset_for_wall(&self, wall_secs: i64) -> i32 {
        let Zone::Named(tz) = self else {
            return self.offset_at(wall_secs);
        };
        let Ok(ts) = Timestamp::from_second(wall_secs) else {
            return 0;
        };
        let wall = TimeZone::UTC.to_datetime(ts);
        let offset = match tz.to_ambiguous_timestamp(wall).offset() {
            AmbiguousOffset::Unambiguous { offset } => offset,
            AmbiguousOffset::Gap { before, .. } | AmbiguousOffset::Fold { before, .. } => before,
        };
        offset.seconds() / 60
    }

    /// `America/Denver` or `-07:00`.
    pub fn name(&self) -> String {
        match self {
            Zone::Named(tz) => tz.iana_name().unwrap_or("UTC").to_owned(),
            Zone::Fixed(o) => offset_text(*o),
        }
    }
}

/// An IANA zone name (`America/Denver`; `None` for blank; `Err` otherwise).
pub fn parse_time_zone(s: &str) -> Result<Option<TimeZone>, String> {
    if s.trim().is_empty() {
        return Ok(None);
    }
    pod_core::tz::parse(s).map(Some)
}

/// The zone an iCalendar `TZID` names, when it is an IANA one: quotes and
/// vendor prefixes (`/mozilla.org/20050126_1/America/Denver`) are dropped.
pub fn tzid_zone(tzid: &str) -> Option<TimeZone> {
    let mut rest = tzid.trim().trim_matches('"');
    loop {
        if let Ok(tz) = pod_core::tz::parse(rest) {
            return Some(tz);
        }
        rest = rest.split_once('/')?.1;
    }
}

/// A parsed date, optionally with a time and a UTC offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct When {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    /// `(hour, minute)`; `None`: an all-day date.
    pub time: Option<(u32, u32)>,
    /// Minutes east of UTC, when the source said.
    pub offset_min: Option<i32>,
}

const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Day names as rules write them (`fri`), Monday first.
pub const DAY_NAMES: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant).
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of day `z` since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y as i32, m, d)
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn digits(s: &str, n: usize) -> Option<u32> {
    (s.len() >= n && s.as_bytes()[..n].iter().all(u8::is_ascii_digit))
        .then(|| s[..n].parse().ok())
        .flatten()
}

/// `Z`, `+HH:MM`, `+HHMM` or `+HH` (or nothing: `Some(None)`).
fn parse_offset(s: &str) -> Option<Option<i32>> {
    let s = s.trim();
    if s.is_empty() {
        return Some(None);
    }
    if s.eq_ignore_ascii_case("z") || s.eq_ignore_ascii_case("utc") || s == "GMT" {
        return Some(Some(0));
    }
    let sign = match s.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let rest = s[1..].replace(':', "");
    let (h, m) = match rest.len() {
        2 => (digits(&rest, 2)?, 0),
        4 => (digits(&rest, 2)?, digits(&rest[2..], 2)?),
        _ => return None,
    };
    (h <= 14 && m < 60).then_some(Some(sign * (h as i32 * 60 + m as i32)))
}

impl When {
    /// Read `2026-11-06`, `2026-11-06T19:30[:00][.000][Z|±HH:MM]` (a space
    /// for the `T` too), or the iCalendar forms `20261106` and
    /// `20261106T193000[Z]`. `None` for anything else or an invalid date.
    pub fn parse(s: &str) -> Option<When> {
        let s = s.trim();
        let (date, rest) = if s.len() >= 10 && s.as_bytes()[4] == b'-' {
            (
                (
                    digits(s, 4)? as i32,
                    digits(&s[5..], 2)?,
                    digits(&s[8..], 2)?,
                ),
                &s[10..],
            )
        } else if s.len() >= 8 && digits(s, 8).is_some() {
            (
                (
                    digits(s, 4)? as i32,
                    digits(&s[4..], 2)?,
                    digits(&s[6..], 2)?,
                ),
                &s[8..],
            )
        } else {
            return None;
        };
        let (year, month, day) = date;
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return None;
        }
        let mut w = When {
            year,
            month,
            day,
            time: None,
            offset_min: None,
        };
        let Some(t) = rest.strip_prefix(['T', 't', ' ']) else {
            return rest.trim().is_empty().then_some(w);
        };
        let h = digits(t, 2)?;
        let after_h = &t[2..];
        let (m, mut tail) = if let Some(x) = after_h.strip_prefix(':') {
            (digits(x, 2)?, &x[2..])
        } else {
            (digits(after_h, 2)?, &after_h[2..])
        };
        // Seconds and fractions are dropped.
        if let Some(x) = tail.strip_prefix(':') {
            digits(x, 2)?;
            tail = &x[2..];
        } else if digits(tail, 2).is_some() {
            tail = &tail[2..];
        }
        if let Some(x) = tail.strip_prefix('.') {
            tail = x.trim_start_matches(|c: char| c.is_ascii_digit());
        }
        if h > 23 || m > 59 {
            return None;
        }
        w.time = Some((h, m));
        w.offset_min = parse_offset(tail)?;
        Some(w)
    }

    /// Seconds since the epoch of this wall clock read as UTC.
    fn wall_secs(&self) -> i64 {
        let (h, m) = self.time.unwrap_or((0, 0));
        days_from_civil(self.year, self.month, self.day) * 86_400
            + i64::from(h) * 3600
            + i64::from(m) * 60
    }

    /// Unix seconds: exact with an offset; else in `zone` (the target's),
    /// else as UTC.
    pub fn timestamp(&self, zone: Option<&Zone>) -> i64 {
        let wall = self.wall_secs();
        let off = self
            .offset_min
            .or(zone.map(|z| z.offset_for_wall(wall)))
            .unwrap_or(0);
        wall - i64::from(off) * 60
    }

    /// This wall clock with the offset `zone` has at that time (a time
    /// without an offset only; else unchanged).
    pub fn placed_in(&self, zone: &Zone) -> When {
        match (self.offset_min, self.time) {
            (None, Some(_)) => When {
                offset_min: Some(zone.offset_for_wall(self.wall_secs())),
                ..*self
            },
            _ => *self,
        }
    }

    /// The same instant on the wall clock of `to` (only when this one's
    /// offset is known; else unchanged).
    pub fn shifted_to(&self, to: Option<&Zone>) -> When {
        let (Some(from), Some(zone), Some(_)) = (self.offset_min, to, self.time) else {
            return *self;
        };
        let instant = self.wall_secs() - i64::from(from) * 60;
        let to = zone.offset_at(instant);
        let secs = instant + i64::from(to) * 60;
        let (y, mo, d) = civil_from_days(secs.div_euclid(86_400));
        let rem = secs.rem_euclid(86_400);
        When {
            year: y,
            month: mo,
            day: d,
            time: Some(((rem / 3600) as u32, ((rem % 3600) / 60) as u32)),
            offset_min: Some(to),
        }
    }

    /// Monday = 0.
    pub fn weekday(&self) -> usize {
        // 1970-01-01 was a Thursday.
        (days_from_civil(self.year, self.month, self.day) + 3).rem_euclid(7) as usize
    }

    /// `2026-11-06` or `2026-11-06T19:30`: the wall clock, sortable.
    pub fn iso(&self) -> String {
        match self.time {
            Some((h, m)) => format!(
                "{:04}-{:02}-{:02}T{h:02}:{m:02}",
                self.year, self.month, self.day
            ),
            None => format!("{:04}-{:02}-{:02}", self.year, self.month, self.day),
        }
    }

    /// `2026-11-06T19:30-07:00` (the offset when known): what we store.
    pub fn stored(&self) -> String {
        match (self.time, self.offset_min) {
            (Some(_), Some(o)) => {
                let sign = if o < 0 { '-' } else { '+' };
                format!(
                    "{}{sign}{:02}:{:02}",
                    self.iso(),
                    o.abs() / 60,
                    o.abs() % 60
                )
            }
            _ => self.iso(),
        }
    }

    /// `Fri Nov 6`.
    pub fn date_text(&self) -> String {
        format!(
            "{} {} {}",
            DAYS[self.weekday()],
            MONTHS[(self.month - 1) as usize],
            self.day
        )
    }

    /// `7:30 PM`, `7 PM`; `None` for an all-day date.
    pub fn time_text(&self) -> Option<String> {
        let (h, m) = self.time?;
        let suffix = if h < 12 { "AM" } else { "PM" };
        let h12 = match h % 12 {
            0 => 12,
            x => x,
        };
        Some(if m == 0 {
            format!("{h12} {suffix}")
        } else {
            format!("{h12}:{m:02} {suffix}")
        })
    }
}

/// `-07:00` style offsets for targets (`None` for blank; `Err` otherwise).
pub fn parse_utc_offset(s: &str) -> Result<Option<i32>, String> {
    if s.trim().is_empty() {
        return Ok(None);
    }
    match parse_offset(s) {
        Some(Some(o)) => Ok(Some(o)),
        _ => Err(format!("\"{s}\" is not a UTC offset like -07:00")),
    }
}

/// `-07:00`.
pub fn offset_text(o: i32) -> String {
    let sign = if o < 0 { '-' } else { '+' };
    format!("{sign}{:02}:{:02}", o.abs() / 60, o.abs() % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_usual_forms() {
        let w = When::parse("2026-11-06T19:30:00-07:00").unwrap();
        assert_eq!(
            (w.year, w.month, w.day, w.time),
            (2026, 11, 6, Some((19, 30)))
        );
        assert_eq!(w.offset_min, Some(-420));
        assert_eq!(w.date_text(), "Fri Nov 6");
        assert_eq!(w.time_text().as_deref(), Some("7:30 PM"));
        assert_eq!(w.stored(), "2026-11-06T19:30-07:00");
        let z = When::parse("20261107T023000Z").unwrap();
        assert_eq!(z.timestamp(None), w.timestamp(None));
        assert_eq!(
            z.shifted_to(Some(&Zone::Fixed(-420))).iso(),
            "2026-11-06T19:30"
        );
        assert_eq!(When::parse("20261106").unwrap().time, None);
        assert_eq!(
            When::parse("2026-11-06 19:00")
                .unwrap()
                .time_text()
                .as_deref(),
            Some("7 PM")
        );
        assert_eq!(
            When::parse("2026-11-06T12:05:00.000+0000")
                .unwrap()
                .offset_min,
            Some(0)
        );
        for bad in [
            "",
            "Friday",
            "2026-02-30",
            "2026-11-06T25:00",
            "2026-11-06Tx",
        ] {
            assert_eq!(When::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn civil_round_trip() {
        for z in [-1000, 0, 365, 20_000, 30_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(When::parse("1970-01-01").unwrap().weekday(), 3);
    }

    #[test]
    fn offsets() {
        assert_eq!(parse_utc_offset("-07:00"), Ok(Some(-420)));
        assert_eq!(parse_utc_offset(""), Ok(None));
        assert!(parse_utc_offset("Denver").is_err());
        assert_eq!(offset_text(-420), "-07:00");
    }

    fn denver() -> Zone {
        Zone::Named(pod_core::tz::parse("America/Denver").unwrap())
    }

    #[test]
    fn named_zones_follow_daylight_saving() {
        let denver_zone = pod_core::tz::parse("America/Denver").unwrap();
        assert_eq!(parse_time_zone("America/Denver"), Ok(Some(denver_zone)));
        let d = &denver();
        assert_eq!(parse_time_zone(" "), Ok(None));
        assert!(parse_time_zone("-07:00").is_err());
        assert_eq!(d.name(), "America/Denver");
        // A wall clock without an offset: MDT in July, MST in December.
        let july = When::parse("2026-07-10T19:30").unwrap();
        let dec = When::parse("2026-12-10T19:30").unwrap();
        assert_eq!(
            july.timestamp(Some(d)),
            When::parse("2026-07-11T01:30Z").unwrap().timestamp(None)
        );
        assert_eq!(
            dec.timestamp(Some(d)),
            When::parse("2026-12-11T02:30Z").unwrap().timestamp(None)
        );
        assert_eq!(july.placed_in(d).stored(), "2026-07-10T19:30-06:00");
        // A UTC time shows on the target's wall clock, in either season.
        let z = When::parse("2026-07-11T01:30:00Z").unwrap();
        assert_eq!(z.shifted_to(Some(d)).stored(), "2026-07-10T19:30-06:00");
        let z = When::parse("2026-12-11T02:30:00Z").unwrap();
        assert_eq!(
            z.shifted_to(Some(d)).time_text().as_deref(),
            Some("7:30 PM")
        );
        // Skipped (spring forward) and repeated (fall back) hours.
        let gap = When::parse("2026-03-08T02:30").unwrap();
        assert_eq!(gap.placed_in(d).offset_min, Some(-420));
        let twice = When::parse("2026-11-01T01:30").unwrap();
        assert_eq!(twice.placed_in(d).offset_min, Some(-360));
    }

    #[test]
    fn tzids() {
        let denver = Some(pod_core::tz::parse("America/Denver").unwrap());
        assert_eq!(tzid_zone("America/Denver"), denver.clone());
        assert_eq!(tzid_zone("\"America/Denver\""), denver.clone());
        assert_eq!(tzid_zone("/mozilla.org/20050126_1/America/Denver"), denver);
        assert_eq!(tzid_zone("Mountain Standard Time"), None);
    }
}
