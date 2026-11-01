//! Android only: forward the process's stdout/stderr to logcat (tag `CanvasEngine`).
//!
//! client-core logs with `println!`/`eprintln!`. On Android, fds 1 and 2 go to
//! `/dev/null`, so those lines are lost. [`init`] points each fd at a pipe and
//! starts a thread that writes every line it reads to logcat: stdout at INFO and
//! stderr at WARN. View them with `adb logcat -s CanvasEngine:V`.
//! Runs once per process. On failure it leaves the fds unchanged.

use std::ffi::{CString, c_char, c_int};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::fd::FromRawFd;
use std::sync::Once;

const TAG: &str = "CanvasEngine";
const ANDROID_LOG_INFO: c_int = 4;
const ANDROID_LOG_WARN: c_int = 5;

#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

/// Redirect stdout and stderr to logcat. Idempotent.
pub fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        redirect(libc::STDOUT_FILENO, ANDROID_LOG_INFO);
        redirect(libc::STDERR_FILENO, ANDROID_LOG_WARN);
    });
}

fn redirect(target_fd: c_int, prio: c_int) {
    let mut fds = [0 as c_int; 2];
    // SAFETY: `fds` is a valid 2-element buffer. O_CLOEXEC keeps the pipe out of
    // child processes; dup2 below clears it on `target_fd`, as intended.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return;
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    // SAFETY: `read_fd` was just created by pipe() and is owned only by this File.
    let reader = unsafe { File::from_raw_fd(read_fd) };

    // Start the reader before redirecting, so writers never block on a pipe
    // that nothing drains.
    let spawned = std::thread::Builder::new()
        .name(format!("logcat-fd{target_fd}"))
        .spawn(move || pump(reader, prio));
    if spawned.is_err() {
        // SAFETY: `write_fd` is ours; the reader was dropped with the closure.
        unsafe { libc::close(write_fd) };
        return;
    }

    // SAFETY: plain fd operations on fds we own. dup2 keeps the pipe open through
    // `target_fd`, so the original write end can be closed either way.
    unsafe {
        libc::dup2(write_fd, target_fd);
        libc::close(write_fd);
    }
}

fn pump(reader: File, prio: c_int) {
    let tag = CString::new(TAG).expect("tag has no NUL");
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                while buf.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                    buf.pop();
                }
                buf.retain(|b| *b != 0);
                let Ok(text) = CString::new(std::mem::take(&mut buf)) else {
                    continue;
                };
                // SAFETY: both pointers are valid NUL-terminated strings.
                unsafe { __android_log_write(prio, tag.as_ptr(), text.as_ptr()) };
            }
        }
    }
}
