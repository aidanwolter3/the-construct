//! Which build a binary is (decision `device-versions`). The server, the
//! web client and the apps are built from the same commits, so they share
//! one build id: `<version>+<commit time, UTC, YYYYMMDDHHMMSS>.<short
//! hash>[.dirty]`, e.g. `0.6.0+20261118153000.c41d9e2`. The commit time
//! orders builds, including ones from a branch the server never ran.
//!
//! The id is compiled in from `CONSTRUCT_BUILD` (made by
//! `scripts/build_id.sh`; CI, the image and the app scripts set it). A build
//! without it is `<crate version>+dev`, which has no time and so no order.

use std::cmp::Ordering;

/// What a client is and which build it runs: `<platform> <build id>`
/// (e.g. `android 0.6.0+20261118153000.c41d9e2`), on every pod request
/// that carries [`crate::DEVICE_HEADER`].
pub const BUILD_HEADER: &str = "x-construct-build";
/// A name for the device the client found itself (`Pixel 9`, a Mac's
/// name, `Chrome on Windows`), shown until the user names it.
pub const DEVICE_NAME_HEADER: &str = "x-construct-device-name";

/// This binary's build id.
pub fn build() -> &'static str {
    match option_env!("CONSTRUCT_BUILD") {
        Some(b) if !b.trim().is_empty() => b,
        _ => concat!(env!("CARGO_PKG_VERSION"), "+dev"),
    }
}

/// The platform this binary is built for, as [`BUILD_HEADER`] names it:
/// `android`, `macos`, `ios`, `windows`, `linux` or `web` (a browser).
pub fn platform() -> &'static str {
    if cfg!(target_arch = "wasm32") {
        "web"
    } else if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

/// This binary's [`BUILD_HEADER`] value.
pub fn build_header() -> String {
    format!("{} {}", platform(), build())
}

/// The platforms a [`BUILD_HEADER`] may name.
pub const PLATFORMS: [&str; 7] = ["android", "ios", "macos", "windows", "linux", "web", "sim"];

/// A [`BUILD_HEADER`] value read: `(platform, build id)`. `None` for a
/// platform not in [`PLATFORMS`] or a build id that is too long or has
/// characters a build id never has.
pub fn parse_build_header(v: &str) -> Option<(&str, &str)> {
    let (platform, build) = v.trim().split_once(' ')?;
    let build = build.trim();
    let ok = PLATFORMS.contains(&platform)
        && !build.is_empty()
        && build.len() <= 80
        && build
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+.-_".contains(&b));
    ok.then_some((platform, build))
}

/// A build id's parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildId {
    pub version: String,
    /// The commit time, `YYYYMMDDHHMMSS` as a number; `None` for a `+dev`
    /// build or anything else without one.
    pub stamp: Option<u64>,
    /// The short commit hash.
    pub commit: Option<String>,
    /// Built from uncommitted changes.
    pub dirty: bool,
}

impl BuildId {
    pub fn parse(id: &str) -> BuildId {
        let (version, meta) = id.split_once('+').unwrap_or((id, ""));
        let mut parts = meta.split('.');
        let stamp = parts
            .next()
            .filter(|s| s.len() == 14 && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse().ok());
        let (commit, dirty) = match stamp {
            Some(_) => {
                let commit = parts.next().filter(|c| !c.is_empty()).map(str::to_owned);
                (commit, parts.any(|p| p == "dirty"))
            }
            None => (None, false),
        };
        BuildId {
            version: version.to_owned(),
            stamp,
            commit,
            dirty,
        }
    }

    /// Which is newer: by commit time; the same time and commit is the same
    /// build. `None` when either has no time.
    pub fn order(&self, other: &BuildId) -> Option<Ordering> {
        let (a, b) = (self.stamp?, other.stamp?);
        Some(match a.cmp(&b) {
            Ordering::Equal if self.commit != other.commit || self.dirty != other.dirty => {
                // Two commits made in the same second: the clean one first.
                self.dirty.cmp(&other.dirty)
            }
            o => o,
        })
    }

    /// The commit date, `(year, month, day, hour, minute)` in UTC.
    pub fn date(&self) -> Option<(u32, u32, u32, u32, u32)> {
        let s = self.stamp?;
        Some((
            (s / 10_000_000_000) as u32,
            (s / 100_000_000 % 100) as u32,
            (s / 1_000_000 % 100) as u32,
            (s / 10_000 % 100) as u32,
            (s / 100 % 100) as u32,
        ))
    }

    /// A short name for people: `Nov 18 · c41d9e2` (`local` added for a
    /// dirty build), or the id itself when it has no time.
    pub fn label(&self) -> String {
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        match (self.date(), &self.commit) {
            (Some((_, m, d, _, _)), Some(c)) => {
                let month = MONTHS.get(m.saturating_sub(1) as usize).unwrap_or(&"?");
                let local = if self.dirty { " local" } else { "" };
                format!("{month} {d} · {c}{local}")
            }
            _ if self.version.is_empty() => "unknown".into(),
            _ => format!("{} (dev)", self.version),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_id_reads_and_orders_by_commit_time() {
        let a = BuildId::parse("0.6.0+20261118153000.c41d9e2");
        assert_eq!(a.stamp, Some(20261118153000));
        assert_eq!(a.commit.as_deref(), Some("c41d9e2"));
        assert!(!a.dirty);
        assert_eq!(a.label(), "Nov 18 · c41d9e2");
        let b = BuildId::parse("0.6.0+20261119080000.f02b77e.dirty");
        assert!(b.dirty);
        assert_eq!(b.label(), "Nov 19 · f02b77e local");
        assert_eq!(a.order(&b), Some(Ordering::Less));
        assert_eq!(a.order(&a.clone()), Some(Ordering::Equal));
        let dev = BuildId::parse("0.1.0+dev");
        assert_eq!(dev.stamp, None);
        assert_eq!(a.order(&dev), None);
        assert_eq!(dev.label(), "0.1.0 (dev)");
        assert_eq!(BuildId::parse("").label(), "unknown");
    }

    #[test]
    fn the_build_header_names_a_known_platform() {
        assert_eq!(
            parse_build_header("android 0.6.0+20261118153000.c41d9e2"),
            Some(("android", "0.6.0+20261118153000.c41d9e2"))
        );
        assert_eq!(parse_build_header("toaster 1.0"), None);
        assert_eq!(parse_build_header("web 1.0 <script>"), None);
        assert_eq!(parse_build_header("web"), None);
        assert!(parse_build_header(&build_header()).is_some());
    }
}
