//! Security audit #31 (L2): the binary refuses to serve the open pod where
//! anyone but this machine could reach it.

use std::process::{Command, Stdio};

fn start(addr: &str, envs: &[(&str, &str)]) -> std::process::Output {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_server"));
    cmd.env("CONSTRUCT_ADDR", addr)
        .env("CONSTRUCT_DATA_DIR", dir.path())
        .env_remove("FLY_APP_NAME")
        .env_remove(server::INSECURE_OPEN_POD_VAR)
        .stdin(Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    // Every case here fails before binding, so it exits on its own.
    cmd.output().unwrap()
}

#[test]
fn an_open_pod_on_the_network_is_refused() {
    let out = start("0.0.0.0:0", &[("CONSTRUCT_OPEN_POD", "dev")]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(server::INSECURE_OPEN_POD_VAR), "{err}");
}

#[test]
fn an_open_pod_behind_fly_is_refused() {
    let out = start(
        "127.0.0.1:0",
        &[
            ("CONSTRUCT_OPEN_POD", "dev"),
            ("FLY_APP_NAME", "x"),
            (server::INSECURE_OPEN_POD_VAR, "1"),
        ],
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("FLY_APP_NAME"), "{err}");
}
