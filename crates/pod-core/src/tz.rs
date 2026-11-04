//! The pod owner's time zone (decision `collection-reminders`): one IANA
//! zone name (`America/Denver`) kept as a pod setting, for adapters that
//! turn a date or a wall-clock time without a zone into an instant (a
//! reminder "on the due date at 09:00"). Unset, the pod reads times as UTC.
//! The zone database is built in, so servers need none of their own.

pub use jiff;
use jiff::tz::TimeZone;

use crate::store::Store;

/// The pod setting holding the zone's IANA name.
pub const TIME_ZONE_SETTING: &str = "time_zone";

/// The zone `name` names: an IANA name such as `Europe/Berlin`, or `UTC`.
pub fn parse(name: &str) -> Result<TimeZone, String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err("a time zone is an IANA name such as America/Denver".into());
    }
    TimeZone::get(name).map_err(|_| {
        format!(
            "'{name}' is not a time zone; use an IANA name such as America/Denver or Europe/Berlin"
        )
    })
}

/// The pod's zone name, when one is set (and still known).
pub fn time_zone_name(store: &Store) -> rusqlite::Result<Option<String>> {
    Ok(store
        .pod_setting(TIME_ZONE_SETTING)?
        .filter(|n| parse(n).is_ok()))
}

/// The pod's zone (UTC when none is set).
pub fn time_zone(store: &Store) -> rusqlite::Result<TimeZone> {
    Ok(time_zone_name(store)?
        .and_then(|n| parse(&n).ok())
        .unwrap_or(TimeZone::UTC))
}

/// Set the pod's zone; answers the zone's canonical name.
pub fn set_time_zone(store: &Store, name: &str) -> Result<String, String> {
    let tz = parse(name)?;
    let canonical = tz.iana_name().unwrap_or(name.trim()).to_owned();
    store
        .set_pod_setting(TIME_ZONE_SETTING, &canonical)
        .map_err(|e| format!("the setting could not be saved: {e}"))?;
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve_with_daylight_saving() {
        let denver = parse("America/Denver").unwrap();
        let july: jiff::Timestamp = "2026-07-01T18:00:00Z".parse().unwrap();
        let december: jiff::Timestamp = "2026-12-01T18:00:00Z".parse().unwrap();
        assert_eq!(july.to_zoned(denver.clone()).hour(), 12);
        assert_eq!(december.to_zoned(denver).hour(), 11);
        assert!(parse("UTC").is_ok());
        assert!(parse("Mars/Olympus").is_err());
        assert!(parse("-07:00").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn the_setting_defaults_to_utc() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(time_zone(&store).unwrap(), TimeZone::UTC);
        assert_eq!(
            set_time_zone(&store, " Europe/Berlin ").unwrap(),
            "Europe/Berlin"
        );
        assert_eq!(
            time_zone_name(&store).unwrap().as_deref(),
            Some("Europe/Berlin")
        );
        assert!(set_time_zone(&store, "nowhere").is_err());
        assert_eq!(
            time_zone_name(&store).unwrap().as_deref(),
            Some("Europe/Berlin")
        );
    }
}
