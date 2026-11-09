//! Display text from the runner, which is untrusted: plain text only,
//! control characters dropped and length capped; the only link kept is a
//! pull request's, checked to be `https://github.com/<owner>/<repo>/pull/<n>`.

/// `s` as one plain line of at most `max` characters (newlines become
/// spaces, other control characters go).
pub fn clean(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut last_space = false;
    for c in s.chars() {
        let c = if c == '\n' || c == '\t' || c == '\r' {
            ' '
        } else {
            c
        };
        if c.is_control() || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}') {
            continue;
        }
        if c == ' ' && last_space {
            continue;
        }
        last_space = c == ' ';
        out.push(c);
    }
    let out = out.trim();
    if out.chars().count() <= max {
        return out.to_owned();
    }
    let mut cut: String = out.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// `s` as text of up to `max` characters that keeps its line breaks (a
/// summary): each line [`clean`]ed, at most 3 blank lines in a row.
pub fn clean_lines(s: &str, max: usize) -> String {
    let lines: Vec<String> = s.lines().map(|l| clean(l, max)).collect();
    let joined = lines.join("\n");
    let joined = joined.trim();
    if joined.chars().count() <= max {
        return joined.to_owned();
    }
    let mut cut: String = joined.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// `url` when it is a GitHub pull request's address.
pub fn pr_url(url: &str) -> Option<String> {
    let rest = url.trim().strip_prefix("https://github.com/")?;
    let parts: Vec<&str> = rest.split('/').collect();
    let name = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    };
    match parts.as_slice() {
        [owner, repo, "pull", n]
            if name(owner)
                && name(repo)
                && !n.is_empty()
                && n.len() <= 10
                && n.bytes().all(|b| b.is_ascii_digit()) =>
        {
            Some(format!("https://github.com/{owner}/{repo}/pull/{n}"))
        }
        _ => None,
    }
}

/// The Claude app's address of session `id`, when the id looks like a
/// Claude Code session's (`session_` and letters or digits).
pub fn claude_url(id: &str) -> Option<String> {
    let rest = id.strip_prefix("session_")?;
    (!rest.is_empty() && rest.len() <= 64 && rest.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then(|| format!("https://claude.ai/code/{id}"))
}

/// A Claude app session link the runner sent, kept only as
/// `https://claude.ai/code/` and a path of plain segments (letters, digits,
/// `_`, `-`), at most 300 characters.
pub fn claude_link(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://claude.ai/code/")?;
    let plain = |seg: &str| {
        !seg.is_empty()
            && seg
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    (url.len() <= 300 && rest.split('/').all(plain)).then(|| url.to_owned())
}

/// A length of time, e.g. `45 s`, `12 min`, `3 h 5 min`, `2 days`.
pub fn duration(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        s if s < 60 => format!("{s} s"),
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 86_400 => match (s % 3600) / 60 {
            0 => format!("{} h", s / 3600),
            m => format!("{} h {m} min", s / 3600),
        },
        s if s < 2 * 86_400 => "1 day".to_owned(),
        s => format!("{} days", s / 86_400),
    }
}

/// How long ago `then_ms` was at `now_ms`: `just now`, `5 min ago`.
pub fn ago(then_ms: i64, now_ms: i64) -> String {
    let secs = (now_ms - then_ms) / 1000;
    if secs < 60 {
        "just now".to_owned()
    } else {
        format!("{} ago", duration(secs))
    }
}

/// How long until `then_ms`: `in 5 min`, or `now` once it's due.
pub fn until(then_ms: i64, now_ms: i64) -> String {
    let secs = (then_ms - now_ms) / 1000;
    if secs < 60 {
        "now".to_owned()
    } else {
        format!("in {}", duration(secs))
    }
}

/// A cost in US dollars: `$0.42`.
pub fn cost(usd: f64) -> String {
    if usd <= 0.0 || !usd.is_finite() {
        return String::new();
    }
    if usd < 0.01 {
        "<$0.01".to_owned()
    } else {
        format!("${usd:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_cleaned() {
        assert_eq!(clean("  a\n\nb\u{7}\u{202E}c  ", 50), "a bc");
        assert_eq!(clean("abcdef", 4), "abc…");
        assert_eq!(clean_lines("one\r\ntwo\u{1b}[31m", 50), "one\ntwo[31m");
    }

    #[test]
    fn only_pull_request_links_are_kept() {
        assert_eq!(
            pr_url("https://github.com/owner/repo/pull/12").as_deref(),
            Some("https://github.com/owner/repo/pull/12")
        );
        for bad in [
            "http://github.com/o/r/pull/1",
            "https://github.com/o/r/issues/1",
            "https://github.com.evil.example/o/r/pull/1",
            "https://github.com/o/r/pull/1?x=y",
            "https://github.com/o/r/pull/abc",
            "javascript:alert(1)",
        ] {
            assert!(pr_url(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn times_read_well() {
        assert_eq!(duration(42), "42 s");
        assert_eq!(duration(3 * 3600 + 300), "3 h 5 min");
        assert_eq!(ago(0, 30_000), "just now");
        assert_eq!(ago(0, 600_000), "10 min ago");
        assert_eq!(until(7_200_000, 0), "in 2 h");
        assert_eq!(cost(0.4213), "$0.42");
        assert_eq!(cost(0.0), "");
        assert_eq!(
            claude_url("session_01Abc").as_deref(),
            Some("https://claude.ai/code/session_01Abc")
        );
        assert!(claude_url("../x").is_none());
        assert_eq!(
            claude_link("https://claude.ai/code/session_01Ab").as_deref(),
            Some("https://claude.ai/code/session_01Ab")
        );
        for bad in [
            "https://claude.ai/code/",
            "https://claude.ai/code/a/../b",
            "https://claude.ai/code/a?x=1",
            "https://claude.ai.evil.example/code/a",
            "http://claude.ai/code/a",
        ] {
            assert!(claude_link(bad).is_none(), "{bad}");
        }
    }
}
