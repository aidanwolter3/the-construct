//! Server push channel (M7, docs/DECISIONS.md D24): a `canvas-engine-events`
//! thread keeps `GET /v1/workspaces/<id>/events` (Server-Sent Events) open
//! while the engine is online and refetches the workspace when a
//! `layout` event names a version newer than the last applied one.
//!
//! The stream is read with async reqwest on a private current-thread Tokio
//! runtime, in short slices ([`TICK`]), so a dropped engine is noticed
//! within a tick and the connection closed. The thread holds only a
//! `Weak<Inner>` while reading; the refetch runs outside the runtime (it
//! uses blocking reqwest).

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{Inner, WorkerSignal, lock};

/// Reconnect delay after the first failure; doubled per failure.
pub const RECONNECT_MIN: Duration = Duration::from_secs(1);
/// Upper bound of the reconnect delay.
pub const RECONNECT_MAX: Duration = Duration::from_secs(30);
/// No bytes at all (not even the server's 15 s `: keepalive`) for this long:
/// the connection is considered dead and reopened.
pub const READ_TIMEOUT: Duration = Duration::from_secs(45);
/// Read slice: how quickly a dropped engine or a session change is noticed.
const TICK: Duration = Duration::from_millis(200);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Response headers must arrive within this long after connecting.
const HEADERS_TIMEOUT: Duration = Duration::from_secs(15);

/// One dispatched SSE event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field; `"message"` when absent.
    pub event: String,
    /// The `data:` lines joined with `\n`.
    pub data: String,
}

/// Incremental `text/event-stream` parser (WHATWG HTML §9.2.6): lines end
/// in LF, CRLF or CR (also when split across chunks); `:` lines are
/// comments (keepalives); `field: value` strips one leading space; multiple
/// `data` lines are joined with `\n`; a blank line dispatches the event if
/// it has data; unknown fields (`id`, `retry`, ...) are ignored.
#[derive(Default)]
pub struct SseParser {
    line: Vec<u8>,
    /// The previous chunk ended in CR: skip a leading LF.
    skip_lf: bool,
    event: String,
    data: String,
    has_data: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw bytes; returns the events completed by them.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        let mut out = Vec::new();
        for &b in bytes {
            if std::mem::take(&mut self.skip_lf) && b == b'\n' {
                continue;
            }
            match b {
                b'\r' | b'\n' => {
                    self.skip_lf = b == b'\r';
                    let line = std::mem::take(&mut self.line);
                    if let Some(ev) = self.line_done(&String::from_utf8_lossy(&line)) {
                        out.push(ev);
                    }
                }
                _ => self.line.push(b),
            }
        }
        out
    }

    fn line_done(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            let event = std::mem::take(&mut self.event);
            let data = std::mem::take(&mut self.data);
            if !std::mem::take(&mut self.has_data) {
                return None;
            }
            return Some(SseEvent {
                event: if event.is_empty() { "message".into() } else { event },
                data,
            });
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_owned(),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            _ => {}
        }
        None
    }
}

/// A `layout` event's payload: `(workspace_id, version)`. `None` for other
/// events or unusable data.
pub fn parse_layout_event(ev: &SseEvent) -> Option<(Option<String>, u64)> {
    if ev.event != "layout" {
        return None;
    }
    let v: Value = serde_json::from_str(&ev.data).ok()?;
    let version = v.get("version")?.as_u64()?;
    let ws = v.get("workspace_id").and_then(Value::as_str).map(str::to_owned);
    Some((ws, version))
}

/// Sleep up to `d`, returning false as soon as the engine shuts down.
fn sleep_unless_shut_down(signal: &WorkerSignal, d: Duration) -> bool {
    let deadline = Instant::now() + d;
    loop {
        if signal.is_shut_down() {
            return false;
        }
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        std::thread::sleep((deadline - now).min(TICK));
    }
}

enum ConnectError {
    Status(u16),
    Other(String),
}

/// The events thread. Exits once the engine is dropped.
pub(crate) fn events_thread(weak: Weak<Inner>, signal: Arc<WorkerSignal>, url: String, poll: Duration) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("[CanvasEngine] events: cannot start a runtime: {e}; push channel disabled");
            return;
        }
    };
    let client = match reqwest::Client::builder().connect_timeout(CONNECT_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[CanvasEngine] events: cannot build an HTTP client: {e}; push channel disabled");
            return;
        }
    };
    let mut backoff = RECONNECT_MIN;
    let mut last_error: Option<String> = None;
    loop {
        if signal.is_shut_down() {
            return;
        }
        let (online, token) = match weak.upgrade() {
            Some(inner) => {
                let online = lock(&inner.state).is_online;
                (online, inner.session_token())
            }
            None => return,
        };
        if !online {
            if !sleep_unless_shut_down(&signal, poll) {
                return;
            }
            continue;
        }

        let connected = rt.block_on(async {
            let mut req = client
                .get(&url)
                .header(reqwest::header::ACCEPT, "text/event-stream");
            if let Some(t) = &token {
                req = req.bearer_auth(t);
            }
            match tokio::time::timeout(HEADERS_TIMEOUT, req.send()).await {
                Err(_) => Err(ConnectError::Other("timed out waiting for the response".into())),
                Ok(Err(e)) => Err(ConnectError::Other(e.to_string())),
                Ok(Ok(r)) if r.status().is_success() => Ok(r),
                Ok(Ok(r)) => Err(ConnectError::Status(r.status().as_u16())),
            }
        });
        let error = match connected {
            Ok(mut resp) => {
                if last_error.take().is_some() {
                    println!("[CanvasEngine] events: subscribed to {url}");
                } else {
                    println!("[CanvasEngine] events: connected to {url}");
                }
                match read_stream(&rt, &mut resp, &weak, &signal, token.as_deref(), &mut backoff) {
                    StreamEnd::ShutDown => return,
                    StreamEnd::SessionChanged => {
                        println!("[CanvasEngine] events: session changed; reconnecting");
                        continue;
                    }
                    StreamEnd::Closed(why) => why,
                }
            }
            Err(ConnectError::Status(401)) if token.is_some() => {
                // Same policy as a workspace fetch (D19): forget the stale
                // session, then show the signed-out bundle.
                if let Some(inner) = weak.upgrade()
                    && let Some(snap) = token
                        .as_deref()
                        .and_then(|t| inner.expire_session(t, "events stream"))
                {
                    inner.notify(snap);
                    inner.refresh();
                }
                "HTTP 401".to_string()
            }
            Err(ConnectError::Status(code)) => format!("HTTP {code}"),
            Err(ConnectError::Other(e)) => e,
        };
        if last_error.as_deref() != Some(error.as_str()) {
            eprintln!(
                "[CanvasEngine] events: {url}: {error}; reconnecting in {} s (repeats are not logged)",
                backoff.as_secs_f32()
            );
        }
        last_error = Some(error);
        if !sleep_unless_shut_down(&signal, backoff) {
            return;
        }
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

enum StreamEnd {
    ShutDown,
    SessionChanged,
    Closed(String),
}

/// Read events until the stream ends, the engine is dropped or the session
/// changes. The first received bytes reset the reconnect backoff.
fn read_stream(
    rt: &tokio::runtime::Runtime,
    resp: &mut reqwest::Response,
    weak: &Weak<Inner>,
    signal: &WorkerSignal,
    token: Option<&str>,
    backoff: &mut Duration,
) -> StreamEnd {
    let mut parser = SseParser::new();
    let mut last_data = Instant::now();
    loop {
        // `chunk()` is cancel-safe: a timed-out poll loses no bytes.
        let step = rt.block_on(async { tokio::time::timeout(TICK, resp.chunk()).await });
        if signal.is_shut_down() {
            return StreamEnd::ShutDown;
        }
        match step {
            Err(_) => {
                if last_data.elapsed() >= READ_TIMEOUT {
                    return StreamEnd::Closed(format!(
                        "no data for {} s",
                        READ_TIMEOUT.as_secs()
                    ));
                }
                let Some(inner) = weak.upgrade() else {
                    return StreamEnd::ShutDown;
                };
                if inner.session_token().as_deref() != token {
                    return StreamEnd::SessionChanged;
                }
            }
            Ok(Ok(Some(bytes))) => {
                last_data = Instant::now();
                *backoff = RECONNECT_MIN;
                for ev in parser.feed(&bytes) {
                    let Some(inner) = weak.upgrade() else {
                        return StreamEnd::ShutDown;
                    };
                    inner.on_push_event(&ev);
                }
            }
            Ok(Ok(None)) => return StreamEnd::Closed("stream ended".into()),
            Ok(Err(e)) => return StreamEnd::Closed(e.to_string()),
        }
    }
}

impl Inner {
    /// A pushed event: refetch on a `layout` event for this workspace whose
    /// version is newer than the last applied one (or when none is known).
    /// Blocking (the refetch); called on the events thread.
    fn on_push_event(&self, ev: &SseEvent) {
        let Some((ws, version)) = parse_layout_event(ev) else {
            if ev.event == "layout" {
                eprintln!("[CanvasEngine] events: unusable layout event ignored: {}", ev.data);
            }
            return;
        };
        if ws.as_deref().is_some_and(|w| w != self.workspace_id) {
            return;
        }
        let current = lock(&self.state).layout_version;
        if current.is_some_and(|c| version <= c) {
            return;
        }
        println!(
            "[CanvasEngine] events: layout v{version} pushed (have {}); refetching",
            current.map_or("none".into(), |c| format!("v{c}"))
        );
        if !self.signal.is_shut_down() {
            self.refresh();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(event: &str, data: &str) -> SseEvent {
        SseEvent {
            event: event.into(),
            data: data.into(),
        }
    }

    #[test]
    fn parses_events_comments_and_multiline_data() {
        let mut p = SseParser::new();
        let out = p.feed(
            b": keepalive\n\nevent: layout\ndata: {\"version\":2}\n\n: keepalive\n\n\
              data: a\ndata:b\ndata\nid: 7\nretry: 10\n\n",
        );
        assert_eq!(out, vec![ev("layout", "{\"version\":2}"), ev("message", "a\nb\n")]);
    }

    #[test]
    fn handles_chunk_boundaries_and_line_endings() {
        let input = "event: layout\r\ndata: {\"workspace_id\":\"home\",\"version\":3}\r\n\r\n\
                     event:layout\rdata:{\"version\":4}\r\r";
        let expected = vec![
            ev("layout", "{\"workspace_id\":\"home\",\"version\":3}"),
            ev("layout", "{\"version\":4}"),
        ];
        // Every split point, including between CR and LF.
        for split in 0..input.len() {
            let mut p = SseParser::new();
            let mut out = p.feed(&input.as_bytes()[..split]);
            out.extend(p.feed(&input.as_bytes()[split..]));
            assert_eq!(out, expected, "split at {split}");
        }
        // Byte by byte.
        let mut p = SseParser::new();
        let out: Vec<_> = input.bytes().flat_map(|b| p.feed(&[b])).collect();
        assert_eq!(out, expected);
    }

    #[test]
    fn event_without_data_is_not_dispatched_and_type_resets() {
        let mut p = SseParser::new();
        assert!(p.feed(b"event: layout\n\n").is_empty());
        assert_eq!(p.feed(b"data: x\n\n"), vec![ev("message", "x")]);
    }

    #[test]
    fn layout_event_payload() {
        assert_eq!(
            parse_layout_event(&ev("layout", r#"{"workspace_id":"home","version":5}"#)),
            Some((Some("home".into()), 5))
        );
        assert_eq!(parse_layout_event(&ev("layout", r#"{"version":1}"#)), Some((None, 1)));
        assert_eq!(parse_layout_event(&ev("message", r#"{"version":1}"#)), None);
        assert_eq!(parse_layout_event(&ev("layout", "garbage")), None);
        assert_eq!(parse_layout_event(&ev("layout", r#"{"version":-1}"#)), None);
    }
}
