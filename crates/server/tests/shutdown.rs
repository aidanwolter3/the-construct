//! The server binary ends open SSE streams cleanly on SIGTERM as well as SIGINT:
//! the chunked body gets its terminating chunk, so clients see "stream ended"
//! rather than a cut body (reqwest: "error decoding response body").
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn connect(port: u16) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => return s,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("server never listened on {port}: {e}"),
        }
    }
}

fn sse_ends_cleanly_on(signal: &str) {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_server"))
        .env("CONSTRUCT_ADDR", format!("127.0.0.1:{port}"))
        .env("CONSTRUCT_DATA_DIR", dir.path())
        // A caller without a pod token uses this pod (D46).
        .env("CONSTRUCT_OPEN_POD", "dev")
        .env_remove("ANTHROPIC_API_KEY")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut s = connect(port);
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(
        b"GET /v1/workspaces/pixelfed_demo/events HTTP/1.1\r\nHost: localhost\r\n\
          Accept: text/event-stream\r\n\r\n",
    )
    .unwrap();
    // Wait for the first event so the stream is definitely open.
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    while !String::from_utf8_lossy(&got).contains("event: layout") {
        let n = s.read(&mut buf).expect("first event");
        assert!(n > 0, "closed early: {}", String::from_utf8_lossy(&got));
        got.extend_from_slice(&buf[..n]);
    }
    assert!(String::from_utf8_lossy(&got).contains("transfer-encoding: chunked"));

    let status = Command::new("kill")
        .args([&format!("-{signal}"), &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());

    loop {
        let n = s.read(&mut buf).expect("read after signal");
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
    }
    let text = String::from_utf8_lossy(&got);
    assert!(
        text.ends_with("0\r\n\r\n"),
        "{signal}: body not terminated cleanly: {text:?}"
    );
    let exit = child.wait().unwrap();
    assert!(exit.success(), "{signal}: server exit {exit:?}");
}

#[test]
fn sse_ends_cleanly_on_sigterm() {
    sse_ends_cleanly_on("TERM");
}

#[test]
fn sse_ends_cleanly_on_sigint() {
    sse_ends_cleanly_on("INT");
}
