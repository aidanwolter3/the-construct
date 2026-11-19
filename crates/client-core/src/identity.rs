//! What a device says about itself on every pod request (decision
//! `device-versions`): beside its id ([`protocol::DEVICE_HEADER`]), which
//! build it runs ([`protocol::BUILD_HEADER`]) and a name for it
//! ([`protocol::DEVICE_NAME_HEADER`]), so the pod can tell the user's
//! devices apart and say which are on the latest build.

use std::sync::OnceLock;

use reqwest::header::{HeaderMap, HeaderValue};

static NAME: OnceLock<String> = OnceLock::new();
static PLATFORM: OnceLock<&'static str> = OnceLock::new();

/// Name this device (a client that knows better than the default, e.g.
/// the Android app with the phone's model). Only the first call counts.
pub fn set_device_name(name: &str) {
    let _ = NAME.set(ascii(name));
}

/// Say which platform this client is when the build target doesn't (the
/// simulator says `sim`). Only the first call counts.
pub fn set_platform(platform: &'static str) {
    let _ = PLATFORM.set(platform);
}

/// The device's name: the one set, else what the system calls it.
pub fn device_name() -> &'static str {
    NAME.get_or_init(|| ascii(&system_name()))
}

/// A name as a header carries it: printable ASCII (curly quotes made
/// straight, other characters dropped), one line, at most 60 characters.
fn ascii(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' => '\'',
            c => c,
        })
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(60)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// What the system calls this device, or "".
fn system_name() -> String {
    let run = |cmd: &str, args: &[&str]| {
        std::process::Command::new(cmd)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    };
    if cfg!(target_os = "android") {
        run("getprop", &["ro.product.model"])
    } else if cfg!(target_os = "macos") {
        run("scutil", &["--get", "ComputerName"])
    } else if cfg!(target_os = "windows") {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    } else {
        std::fs::read_to_string("/etc/hostname")
            .map(|h| h.trim().to_owned())
            .unwrap_or_default()
    }
}

/// The build and name headers (the device id is sent where it always was).
pub(crate) fn headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    let platform = PLATFORM.get().copied().unwrap_or(protocol::platform());
    if let Ok(v) = HeaderValue::from_str(&format!("{platform} {}", protocol::build())) {
        h.insert(protocol::BUILD_HEADER, v);
    }
    let name = device_name();
    if !name.is_empty()
        && let Ok(v) = HeaderValue::from_str(name)
    {
        h.insert(protocol::DEVICE_NAME_HEADER, v);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_one_line_of_ascii() {
        assert_eq!(
            ascii(" Aidan\u{2019}s MacBook Air\n"),
            "Aidan's MacBook Air"
        );
        assert_eq!(ascii("Café ☕"), "Caf");
        assert!(headers().contains_key(protocol::BUILD_HEADER));
    }
}
