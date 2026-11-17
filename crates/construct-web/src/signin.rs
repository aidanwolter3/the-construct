//! Signing in to an upstream (Pixelfed) from a browser (decision
//! `web-upstream-sign-in`): the page itself is the redirect. The client
//! registers with the pod for `<origin>/web`, keeps the PKCE verifier and
//! `state` in the tab's `sessionStorage`, and goes to the instance's
//! authorize page; the instance sends the tab back to `/web?code=…&state=…`,
//! and the client hands the code and verifier to the pod, which adds the
//! sign-in to this browser's session. No I/O here, so it is tested natively;
//! `crate::web` does the requests.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The `sessionStorage` key a sign-in waiting for its redirect is kept under.
pub const PENDING_KEY: &str = "construct.sign_in";
/// The page's path, which is also the redirect.
pub const PAGE_PATH: &str = "/web";
/// A sign-in waiting longer than this for its redirect is dropped.
pub const PENDING_MAX_SECS: f64 = 15.0 * 60.0;

pub const STARTING: &str = "Opening the sign-in page…";
pub const FINISHING: &str = "Signing in…";
pub const NOT_OWNER: &str = "This pod belongs to a different Pixelfed account.";
pub const REJECTED: &str = "Pixelfed rejected the sign-in. Please try again.";
pub const INSTANCE_NOT_ALLOWED: &str = "This pod isn't configured for that Pixelfed instance.";
pub const REDIRECT_NOT_ALLOWED: &str = "This pod doesn't accept sign-ins from the web yet.";
pub const UPSTREAM_DOWN: &str = "Can't reach Pixelfed right now.";
pub const POD_UNREACHABLE: &str = "Can't reach your pod.";
pub const DENIED: &str = "Sign-in was cancelled.";
pub const EXPIRED: &str = "That sign-in took too long. Please try again.";
pub const FAILED: &str = "Something went wrong. Please try again.";

/// The redirect the web client registers: its own page.
pub fn redirect_uri(origin: &str) -> String {
    format!("{}{PAGE_PATH}", origin.trim_end_matches('/'))
}

/// The status line for a failed pod call: `status` is `None` when the pod
/// couldn't be reached, `code` the `error` of its answer.
pub fn error_message(status: Option<u16>, code: Option<&str>) -> &'static str {
    let Some(status) = status else {
        return POD_UNREACHABLE;
    };
    match code {
        Some("account_not_owner") => NOT_OWNER,
        Some("oauth_exchange_failed") => REJECTED,
        Some("instance_not_allowed") => INSTANCE_NOT_ALLOWED,
        Some("redirect_uri_not_allowed") => REDIRECT_NOT_ALLOWED,
        Some("upstream_unreachable" | "upstream_error") => UPSTREAM_DOWN,
        _ if status == 502 || status == 504 => UPSTREAM_DOWN,
        _ => FAILED,
    }
}

/// The `action.auth.login` a sign-in intent starts: the intent itself, or
/// for Settings' `action.device` `sign_in` the provider's (from a
/// `GET /v1/auth/sign-ins` answer), on the server typed in `value` when the
/// provider takes another one. `None` for anything else.
pub fn login_intent(intent: &Value, sign_ins: Option<&Value>) -> Option<Value> {
    match (intent["intent"].as_str(), intent["command"].as_str()) {
        (Some("action.auth.login"), _) => Some(intent.clone()),
        (Some("action.device"), Some("sign_in")) => {
            let id = intent["id"].as_str()?;
            let p = sign_ins?["providers"]
                .as_array()?
                .iter()
                .find(|p| p["id"] == id)?;
            let mut start = p["intent"].clone();
            if start["intent"] != "action.auth.login" {
                return None;
            }
            let server = intent["value"].as_str().unwrap_or("").trim();
            if !server.is_empty() && !p["server_placeholder"].is_null() {
                start["instance_url"] = json!(server);
            }
            Some(start)
        }
        _ => None,
    }
}

/// A PKCE verifier (or a `state`) of random bytes: base64url without
/// padding, all RFC 3986 unreserved characters (32 bytes give the 43
/// characters RFC 7636 asks for at least).
pub fn random_string(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// `BASE64URL-NOPAD(SHA256(verifier))` (RFC 7636 S256).
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Percent-encodes everything but RFC 3986 unreserved characters.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => match (hex(b[i + 1]), hex(b[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 3;
                    continue;
                }
                _ => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The authorize page's address.
pub fn authorize_url(
    endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    scope: &str,
    state: &str,
    challenge: &str,
) -> String {
    let query: Vec<String> = [
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("scope", scope),
        ("state", state),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
    ]
    .iter()
    .map(|(k, v)| format!("{k}={}", percent_encode(v)))
    .collect();
    let sep = if endpoint.contains('?') { '&' } else { '?' };
    format!("{endpoint}{sep}{}", query.join("&"))
}

/// A sign-in waiting for its redirect, as kept in `sessionStorage`.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub state: String,
    pub verifier: String,
    pub instance_url: String,
    pub token_path: String,
    pub redirect_uri: String,
    /// When it started (milliseconds since 1970).
    pub started_ms: f64,
}

impl Pending {
    pub fn to_json(&self) -> String {
        json!({ "state": self.state, "verifier": self.verifier, "instance_url": self.instance_url,
                "token_path": self.token_path, "redirect_uri": self.redirect_uri,
                "started_ms": self.started_ms })
        .to_string()
    }

    pub fn parse(raw: &str) -> Option<Pending> {
        let v: Value = serde_json::from_str(raw).ok()?;
        let s = |k: &str| v[k].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        Some(Pending {
            state: s("state")?,
            verifier: s("verifier")?,
            instance_url: s("instance_url")?,
            token_path: s("token_path")?,
            redirect_uri: s("redirect_uri")?,
            started_ms: v["started_ms"].as_f64()?,
        })
    }

    /// The token exchange's body for `code`.
    pub fn exchange(&self, code: &str) -> Value {
        json!({ "instance_url": self.instance_url, "code": code,
                "code_verifier": self.verifier, "redirect_uri": self.redirect_uri })
    }
}

/// What the instance sent the page back with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Return {
    Code { code: String, state: String },
    Error { error: String, state: String },
}

impl Return {
    pub fn state(&self) -> &str {
        match self {
            Return::Code { state, .. } | Return::Error { state, .. } => state,
        }
    }
}

/// The page's query (`location.search`, with or without its `?`) when it is
/// a sign-in's return: `code` or `error`, with `state`.
pub fn parse_return(search: &str) -> Option<Return> {
    let q = search.strip_prefix('?').unwrap_or(search);
    let get = |name: &str| {
        q.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k) == name)
                .then(|| percent_decode(v))
                .filter(|v| !v.is_empty())
        })
    };
    let state = get("state").unwrap_or_default();
    if let Some(error) = get("error") {
        return Some(Return::Error { error, state });
    }
    let code = get("code")?;
    Some(Return::Code { code, state })
}

/// What a return means for a sign-in that may wait for it, at `now_ms`:
/// the code to exchange, or the message to show.
pub fn check_return(
    ret: &Return,
    pending: Option<&Pending>,
    now_ms: f64,
) -> Result<String, &'static str> {
    let Some(p) = pending.filter(|p| p.state == ret.state()) else {
        // Not this tab's sign-in (another tab, an old link, a forged one).
        return Err(FAILED);
    };
    if now_ms - p.started_ms > PENDING_MAX_SECS * 1000.0 {
        return Err(EXPIRED);
    }
    match ret {
        Return::Code { code, .. } => Ok(code.clone()),
        Return::Error { error, .. } if error == "access_denied" => Err(DENIED),
        Return::Error { .. } => Err(REJECTED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending() -> Pending {
        Pending {
            state: "st".into(),
            verifier: "v".repeat(43),
            instance_url: "https://pixelfed.example".into(),
            token_path: "/v1/auth/pixelfed/token".into(),
            redirect_uri: "https://pod.example/web".into(),
            started_ms: 1_000.0,
        }
    }

    #[test]
    fn the_redirect_is_the_page() {
        assert_eq!(
            redirect_uri("https://pod.example"),
            "https://pod.example/web"
        );
        assert_eq!(
            redirect_uri("http://127.0.0.1:8080/"),
            "http://127.0.0.1:8080/web"
        );
    }

    #[test]
    fn rfc7636_appendix_b() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let v = random_string(&[0xff; 32]);
        assert_eq!(v.len(), 43);
        assert!(
            v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        );
    }

    #[test]
    fn the_authorize_url_is_encoded() {
        assert_eq!(
            authorize_url(
                "https://pf.example/oauth/authorize",
                "c 1",
                "https://pod.example/web",
                "read write",
                "s",
                "ch"
            ),
            "https://pf.example/oauth/authorize?response_type=code&client_id=c%201&\
             redirect_uri=https%3A%2F%2Fpod.example%2Fweb&scope=read%20write&state=s&\
             code_challenge=ch&code_challenge_method=S256"
        );
    }

    #[test]
    fn settings_sign_in_uses_the_providers_intent() {
        let answer = json!({ "providers": [
            { "id": "pixelfed", "server_placeholder": "Server",
              "intent": { "intent": "action.auth.login", "instance_url": "https://pf.example",
                          "app_path": "/v1/auth/pixelfed/app", "token_path": "/v1/auth/pixelfed/token" } },
            { "id": "fixed", "intent": { "intent": "action.auth.login", "instance_url": "https://a.example" } },
        ]});
        let tap = |id: &str, value: &str| json!({ "intent": "action.device", "command": "sign_in", "id": id, "value": value });
        let got = login_intent(&tap("pixelfed", ""), Some(&answer)).unwrap();
        assert_eq!(got["instance_url"], "https://pf.example");
        assert_eq!(got["token_path"], "/v1/auth/pixelfed/token");
        let other = login_intent(&tap("pixelfed", " pixelfed.example "), Some(&answer)).unwrap();
        assert_eq!(other["instance_url"], "pixelfed.example");
        let fixed = login_intent(&tap("fixed", "other.example"), Some(&answer)).unwrap();
        assert_eq!(fixed["instance_url"], "https://a.example");
        assert_eq!(login_intent(&tap("nope", ""), Some(&answer)), None);
        assert_eq!(login_intent(&tap("pixelfed", ""), None), None);
        let login = json!({ "intent": "action.auth.login", "instance_url": "https://pf.example" });
        assert_eq!(login_intent(&login, None), Some(login.clone()));
        let out = json!({ "intent": "action.device", "command": "sign_out" });
        assert_eq!(login_intent(&out, Some(&answer)), None);
    }

    #[test]
    fn returns_parse() {
        assert_eq!(
            parse_return("?code=a%2Fb+c&state=xyz"),
            Some(Return::Code {
                code: "a/b c".into(),
                state: "xyz".into()
            })
        );
        assert_eq!(
            parse_return("error=access_denied&state=s&error_description=No"),
            Some(Return::Error {
                error: "access_denied".into(),
                state: "s".into()
            })
        );
        assert_eq!(parse_return("?cpu"), None);
        assert_eq!(parse_return(""), None);
        assert_eq!(parse_return("?code=&state=s"), None);
        assert_eq!(
            parse_return("?code=%E2%9C%93%2"),
            Some(Return::Code {
                code: "✓%2".into(),
                state: "".into()
            })
        );
    }

    #[test]
    fn a_return_is_taken_only_for_this_tabs_sign_in() {
        let p = pending();
        let code = Return::Code {
            code: "c".into(),
            state: "st".into(),
        };
        assert_eq!(check_return(&code, Some(&p), 2_000.0), Ok("c".into()));
        assert_eq!(check_return(&code, None, 2_000.0), Err(FAILED));
        let forged = Return::Code {
            code: "c".into(),
            state: "other".into(),
        };
        assert_eq!(check_return(&forged, Some(&p), 2_000.0), Err(FAILED));
        assert_eq!(
            check_return(&code, Some(&p), 1_000.0 + 16.0 * 60_000.0),
            Err(EXPIRED)
        );
        let denied = Return::Error {
            error: "access_denied".into(),
            state: "st".into(),
        };
        assert_eq!(check_return(&denied, Some(&p), 2_000.0), Err(DENIED));
        let bad = Return::Error {
            error: "server_error".into(),
            state: "st".into(),
        };
        assert_eq!(check_return(&bad, Some(&p), 2_000.0), Err(REJECTED));
    }

    #[test]
    fn pending_round_trips() {
        let p = pending();
        assert_eq!(Pending::parse(&p.to_json()), Some(p.clone()));
        assert_eq!(Pending::parse("{}"), None);
        assert_eq!(Pending::parse("not json"), None);
        assert_eq!(p.exchange("c")["code_verifier"], p.verifier);
    }

    #[test]
    fn errors_read_as_sentences() {
        assert_eq!(error_message(None, None), POD_UNREACHABLE);
        assert_eq!(
            error_message(Some(400), Some("redirect_uri_not_allowed")),
            REDIRECT_NOT_ALLOWED
        );
        assert_eq!(
            error_message(Some(403), Some("account_not_owner")),
            NOT_OWNER
        );
        assert_eq!(error_message(Some(502), None), UPSTREAM_DOWN);
        assert_eq!(error_message(Some(500), None), FAILED);
    }
}
