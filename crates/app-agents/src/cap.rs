//! The runner's side of the wire: the Construct Agent Protocol, v2
//! (decision `agents-runs`).
//!
//! The pod connects out to the runner; the runner never calls the pod.
//! Every request carries the pod's bearer token for the runner. The runner
//! sends whole records with a `version`; the pod keeps the higher one.
//! - `GET /v2/snapshot`: `{v: 2, cursor, runs[], sprites[], schedules[],
//!   checks[]}`, the state now.
//! - `GET /v2/stream?since=<cursor>`: server-sent events with `id: <seq>`,
//!   resumable with `Last-Event-ID`; each is `{v: 2, seq, ts, type,
//!   record}` ([`Event`]).
//! - `GET /v2/runs/<id>/log`: one run's history, read on demand.
//! - `POST /v2/commands`: one of a fixed set ([`Command`]). Errors are JSON
//!   `{error}` with 400, 401, 404, 409 or 502.
//!
//! Everything the runner sends is read leniently (unknown fields ignored,
//! missing ones empty) and treated as untrusted display text.

use std::time::Duration;

use serde_json::{Value, json};

/// The kind of record an [`Event`] carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Run,
    Sprite,
    Schedule,
    Check,
}

/// A CAP v2 event: `{v: 2, seq, ts (ms), type, record}`, `type` one of
/// `run.updated`, `sprite.updated`, `schedule.updated`, `check.updated`.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub seq: i64,
    /// Milliseconds since the epoch.
    pub ts: i64,
    pub kind: Kind,
    /// The whole record.
    pub record: Value,
}

impl Event {
    /// The event in `v` (one SSE message's data), or `None` when it is not a
    /// v2 event with a sequence number, a known type and a record.
    pub fn parse(v: &Value) -> Option<Self> {
        if v.get("v").and_then(Value::as_i64) != Some(2) {
            return None;
        }
        let kind = match v.get("type").and_then(Value::as_str)? {
            "run.updated" => Kind::Run,
            "sprite.updated" => Kind::Sprite,
            "schedule.updated" => Kind::Schedule,
            "check.updated" => Kind::Check,
            _ => return None,
        };
        let record = v.get("record").filter(|r| r.is_object())?.clone();
        Some(Self {
            seq: v.get("seq")?.as_i64()?,
            ts: v.get("ts").and_then(Value::as_i64).unwrap_or(0),
            kind,
            record,
        })
    }
}

/// The commands the pod may send: a fixed set, so a pod (or a leaked pod
/// token) can't send arbitrary shell or configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Start a run (202), or get the task's run that hasn't ended (200).
    /// `resume_from`: an ended run whose conversation the new run continues
    /// with `prompt` as the next message (Reopen).
    StartRun {
        workspace: String,
        task_id: String,
        title: String,
        prompt: String,
        resume_from: Option<String>,
    },
    StopRun {
        run_id: String,
    },
    WakeSprite {
        workspace: String,
        minutes: u32,
    },
    RunSchedule {
        name: String,
    },
    PauseSchedule {
        name: String,
    },
    ResumeSchedule {
        name: String,
    },
}

impl Command {
    /// The request body.
    pub fn body(&self) -> Value {
        match self {
            Self::StartRun {
                workspace,
                task_id,
                title,
                prompt,
                resume_from,
            } => {
                let mut b = json!({"type": "start_run", "workspace": workspace,
                    "task_id": task_id, "title": title, "prompt": prompt});
                if let Some(r) = resume_from {
                    b["resume_from"] = json!(r);
                }
                b
            }
            Self::StopRun { run_id } => json!({"type": "stop_run", "run_id": run_id}),
            Self::WakeSprite { workspace, minutes } => {
                json!({"type": "wake_sprite", "workspace": workspace, "minutes": minutes})
            }
            Self::RunSchedule { name } => json!({"type": "run_schedule", "name": name}),
            Self::PauseSchedule { name } => json!({"type": "pause_schedule", "name": name}),
            Self::ResumeSchedule { name } => json!({"type": "resume_schedule", "name": name}),
        }
    }
}

/// The id of the run in a command's answer (`{run: {id, ...}}`), if it is
/// one the pod can use as a key.
pub fn run_id_of(answer: &Value) -> Option<String> {
    answer
        .get("run")
        .and_then(|r| r.get("id"))
        .and_then(Value::as_str)
        .filter(|id| valid_id(id))
        .map(str::to_owned)
}

/// A run id the pod accepts: 1 to 120 letters, digits, `_` or `-` (it goes
/// in URLs and entity URIs).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 120
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Why a runner request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapError {
    /// No answer: the runner is down, the name doesn't resolve, a timeout.
    Unreachable(String),
    /// The runner refused the pod's token (401).
    Unauthorized,
    /// Any other error status, with the runner's `error` text.
    Status(u16, String),
    /// An answer that isn't CAP v2.
    Invalid(String),
}

impl std::fmt::Display for CapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(e) => write!(f, "the runner can't be reached ({e})"),
            Self::Unauthorized => f.write_str("the runner refused the pod's token"),
            Self::Status(s, e) if e.is_empty() => write!(f, "the runner answered {s}"),
            Self::Status(s, e) => write!(f, "the runner answered {s}: {e}"),
            Self::Invalid(e) => write!(f, "the runner's answer is not CAP v2 ({e})"),
        }
    }
}

/// Where the runner is and how to call it. Never logged: `Debug` shows the
/// host only.
#[derive(Clone, PartialEq, Eq)]
pub struct Runner {
    /// The base URL without a trailing slash, e.g. `https://runner.example`.
    pub url: String,
    pub token: String,
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner")
            .field("host", &host_of(&self.url))
            .finish()
    }
}

/// The host of `url` (for display: never the token).
pub fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str().map(|h| match u.port() {
                Some(p) => format!("{h}:{p}"),
                None => h.to_owned(),
            })
        })
        .unwrap_or_default()
}

/// A runner URL as typed: `Ok(base)` for an `https://` URL of a public host
/// (any `http(s)://` URL when `allow_private`), without path, query or
/// credentials.
pub fn check_url(url: &str, allow_private: bool) -> Result<String, String> {
    let url = url.trim().trim_end_matches('/');
    let parsed =
        reqwest::Url::parse(url).map_err(|_| "the runner's URL is not a URL".to_owned())?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("the runner's URL must not carry a user name or password".into());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() || parsed.path() != "/" {
        return Err("the runner's URL is its base address, e.g. https://runner.example".into());
    }
    let ok = if allow_private {
        matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some()
    } else {
        pod_core::net::is_public_https(&parsed)
    };
    if !ok {
        return Err("the runner's URL must be https:// on a public host name".into());
    }
    Ok(url.to_owned())
}

/// The HTTP clients: one for requests (short timeout), one for the stream
/// (no overall timeout; a quiet stream is dropped after [`STREAM_IDLE`]).
#[derive(Debug, Clone)]
pub struct Clients {
    pub request: reqwest::Client,
    pub stream: reqwest::Client,
}

/// How long a stream may stay silent before the pod reconnects.
pub const STREAM_IDLE: Duration = Duration::from_secs(90);
/// The request timeout.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

impl Clients {
    /// `allow_private`: any address (tests, local development); otherwise
    /// public addresses only. Neither follows redirects: the token must go
    /// to the runner and nowhere else.
    pub fn new(allow_private: bool) -> Self {
        let make = |timeout: Option<Duration>| {
            let b = if allow_private {
                pod_core::net::builder(timeout.unwrap_or(Duration::from_secs(24 * 3600)))
            } else {
                pod_core::net::public_builder(timeout.unwrap_or(Duration::from_secs(24 * 3600)))
            };
            b.redirect(reqwest::redirect::Policy::none())
                .read_timeout(STREAM_IDLE)
                .build()
                .expect("an HTTP client")
        };
        Self {
            request: make(Some(REQUEST_TIMEOUT)),
            stream: make(None),
        }
    }
}

async fn error_of(resp: reqwest::Response) -> CapError {
    let status = resp.status().as_u16();
    if status == 401 {
        return CapError::Unauthorized;
    }
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    let text = body
        .get("error")
        .and_then(Value::as_str)
        .map(|s| crate::text::clean(s, 200))
        .unwrap_or_default();
    CapError::Status(status, text)
}

/// `GET /v2/snapshot`.
pub async fn snapshot(clients: &Clients, runner: &Runner) -> Result<Value, CapError> {
    let resp = clients
        .request
        .get(format!("{}/v2/snapshot", runner.url))
        .bearer_auth(&runner.token)
        .send()
        .await
        .map_err(|e| CapError::Unreachable(short(&e)))?;
    if !resp.status().is_success() {
        return Err(error_of(resp).await);
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| CapError::Invalid(short(&e)))?;
    if !v.is_object() || v.get("v").and_then(Value::as_i64) != Some(2) {
        return Err(CapError::Invalid("not a v2 snapshot".into()));
    }
    Ok(v)
}

/// `GET /v2/runs/<id>/log`: `{entries: [{ts, kind, text}]}`, oldest first.
pub async fn run_log(clients: &Clients, runner: &Runner, id: &str) -> Result<Value, CapError> {
    if !valid_id(id) {
        return Err(CapError::Status(400, "not a run id".into()));
    }
    let resp = clients
        .request
        .get(format!("{}/v2/runs/{id}/log", runner.url))
        .bearer_auth(&runner.token)
        .send()
        .await
        .map_err(|e| CapError::Unreachable(short(&e)))?;
    if !resp.status().is_success() {
        return Err(error_of(resp).await);
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| CapError::Invalid(short(&e)))?;
    if !v.get("entries").is_some_and(Value::is_array) {
        return Err(CapError::Invalid("not a run log".into()));
    }
    Ok(v)
}

/// `POST /v2/commands`: the runner's answer (`{run}`, `{sprite}` or
/// `{schedule}`).
pub async fn command(clients: &Clients, runner: &Runner, cmd: &Command) -> Result<Value, CapError> {
    let resp = clients
        .request
        .post(format!("{}/v2/commands", runner.url))
        .bearer_auth(&runner.token)
        .json(&cmd.body())
        .send()
        .await
        .map_err(|e| CapError::Unreachable(short(&e)))?;
    if !resp.status().is_success() {
        return Err(error_of(resp).await);
    }
    Ok(resp.json().await.unwrap_or_else(|_| json!({"ok": true})))
}

/// `GET /v2/stream?since=<cursor>`: the open response, its body still to
/// read with [`SseParser`].
pub async fn open_stream(
    clients: &Clients,
    runner: &Runner,
    since: i64,
) -> Result<reqwest::Response, CapError> {
    let resp = clients
        .stream
        .get(format!("{}/v2/stream", runner.url))
        .query(&[("since", since.to_string())])
        .header("Last-Event-ID", since.to_string())
        .header("Accept", "text/event-stream")
        .bearer_auth(&runner.token)
        .send()
        .await
        .map_err(|e| CapError::Unreachable(short(&e)))?;
    if !resp.status().is_success() {
        return Err(error_of(resp).await);
    }
    Ok(resp)
}

/// An error without the request URL (which names the runner).
fn short(e: &reqwest::Error) -> String {
    let kind = if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "no connection"
    } else if e.is_decode() {
        "unreadable answer"
    } else {
        "request failed"
    };
    kind.to_owned()
}

/// A server-sent events reader: feed it bytes, take whole messages.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    data: Vec<String>,
    id: Option<String>,
}

/// One SSE message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseMessage {
    pub id: Option<String>,
    pub data: String,
}

/// The most a single SSE line may hold before the stream is dropped.
const MAX_LINE: usize = 256 * 1024;

impl SseParser {
    /// Add `chunk`; returns the messages it completed. `Err` when a line
    /// grows past [`MAX_LINE`].
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseMessage>, String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=i).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line).into_owned();
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(SseMessage {
                        id: self.id.take(),
                        data: self.data.join("\n"),
                    });
                    self.data.clear();
                }
                self.id = None;
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line.as_str(), ""),
            };
            match field {
                "data" => self.data.push(value.to_owned()),
                "id" => self.id = Some(value.to_owned()),
                _ => {}
            }
        }
        if self.buf.len() > MAX_LINE {
            return Err("an event is too long".into());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_messages_split_across_chunks() {
        let mut p = SseParser::default();
        assert!(p.push(b": keepalive\n\nid: 4").unwrap().is_empty());
        let got = p
            .push(b"2\r\ndata: {\"seq\": 42,\ndata:  \"v\": 1}\r\n\r\ndata: x\n")
            .unwrap();
        assert_eq!(
            got,
            [SseMessage {
                id: Some("42".into()),
                data: "{\"seq\": 42,\n \"v\": 1}".into()
            }]
        );
        assert_eq!(
            p.push(b"\n").unwrap(),
            [SseMessage {
                id: None,
                data: "x".into()
            }]
        );
    }

    #[test]
    fn events_parse_leniently() {
        let e = Event::parse(&json!({"v": 2, "seq": 42, "ts": 5, "type": "run.updated",
            "record": {"id": "run_1", "version": 3}, "extra": true}))
        .unwrap();
        assert_eq!((e.seq, e.kind), (42, Kind::Run));
        assert_eq!(e.record["id"], "run_1");
        for bad in [
            json!({"v": 1, "seq": 1, "type": "run.updated", "record": {}}),
            json!({"v": 2, "seq": 1, "type": "session.status", "record": {}}),
            json!({"v": 2, "seq": 1, "type": "run.updated"}),
            json!({"v": 2, "type": "run.updated", "record": {}}),
        ] {
            assert!(Event::parse(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn commands_have_their_v2_bodies() {
        let start = Command::StartRun {
            workspace: "w".into(),
            task_id: "r1".into(),
            title: "t".into(),
            prompt: "p".into(),
            resume_from: None,
        };
        assert!(start.body().get("resume_from").is_none());
        let reopen = Command::StartRun {
            workspace: "w".into(),
            task_id: "r1".into(),
            title: "t".into(),
            prompt: "p".into(),
            resume_from: Some("run_1".into()),
        };
        assert_eq!(reopen.body()["resume_from"], "run_1");
        assert_eq!(
            Command::StopRun {
                run_id: "run_1".into()
            }
            .body(),
            json!({"type": "stop_run", "run_id": "run_1"})
        );
        assert_eq!(
            run_id_of(&json!({"run": {"id": "run_9"}})).as_deref(),
            Some("run_9")
        );
        assert!(run_id_of(&json!({"run": {"id": "../x"}})).is_none());
    }

    #[test]
    fn runner_urls_are_checked() {
        assert_eq!(
            check_url(" https://runner.example/ ", false).as_deref(),
            Ok("https://runner.example")
        );
        for bad in [
            "http://runner.example",
            "https://10.0.0.1",
            "https://localhost",
            "https://u:p@runner.example",
            "https://runner.example/v1",
            "https://runner.example:8443",
            "nope",
        ] {
            assert!(check_url(bad, false).is_err(), "{bad}");
        }
        assert!(check_url("http://127.0.0.1:9000", true).is_ok());
    }

    #[test]
    fn debug_shows_the_host_only() {
        let r = Runner {
            url: "https://runner.example".into(),
            token: "secret-token".into(),
        };
        let s = format!("{r:?}");
        assert!(s.contains("runner.example") && !s.contains("secret"), "{s}");
    }
}
