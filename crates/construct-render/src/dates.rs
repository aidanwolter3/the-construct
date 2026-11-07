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

/// Today's epoch day on this device, in its time zone (UTC when the zone
/// can't be read).
pub fn today() -> i64 {
    let now = jiff::Zoned::now();
    let d = now.date();
    epoch_day(
        i64::from(d.year()),
        i64::from(d.month()),
        i64::from(d.day()),
    )
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
}
