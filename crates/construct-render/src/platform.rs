//! The platform interface: what the app needs from the system it runs on,
//! implemented once per platform (Android, desktop; a web build later), so
//! the engine and the window stay the same everywhere.

/// What a platform gives the app: the soft keyboard, the space its own UI
/// covers (status bar, cutout, navigation bar), the browser, the clipboard,
/// the photo picker, what it tells the app (links, picked photos, messages)
/// and where the app's pod is kept. Android (`android.rs`, with `bridge.rs`)
/// and the desktop viewer implement it; a web build would too. The defaults
/// are a desktop's.
pub trait Platform {
    /// Shows the keyboard while a text field is focused, hides it otherwise.
    fn show_keyboard(&self, show: bool);
    /// Insets in physical pixels: top, right, bottom, left.
    fn insets_px(&self, window_px: (u32, u32)) -> [f32; 4];
    /// Opens an `http(s)` address in the browser.
    fn open_link(&self, url: &str) {
        open_link_here(url);
    }
    /// The platform closed the window for good (Android destroyed the
    /// activity): the event loop ends.
    fn gone(&self) -> bool {
        false
    }
    /// The text on the clipboard, for Paste.
    fn clipboard(&self) -> Option<String> {
        clipboard_here()
    }
    /// Puts text on the clipboard (Copy, Cut).
    fn set_clipboard(&self, text: &str) {
        set_clipboard_here(text);
    }
    /// Opens the platform's photo picker for up to `max` photos; false when
    /// it has none (the desktop's file dialog then opens:
    /// `Session::pick_here`). What is picked comes back as
    /// [`PlatformEvent::Picked`].
    fn pick_images(&self, _max: u32) -> bool {
        false
    }
    /// What the platform has told the app since last asked (a link, photos
    /// picked, a message).
    fn events(&self) -> Vec<PlatformEvent> {
        Vec::new()
    }
    /// Whether the system is in dark mode; `None`: as the window says
    /// (winit's theme).
    fn dark(&self) -> Option<bool> {
        None
    }
    /// The keyboard's own editing (Android's input connection: suggestions,
    /// autocorrect, composing), given the focused field as the engine has it
    /// (`None`: none is focused). Returns what the keyboard did since last
    /// asked, for the window to apply first; else the keyboard is told of the
    /// engine's changes. `None` where the keyboard only sends keys.
    fn text_input(
        &self,
        _field: Option<&crate::ime::TextState>,
    ) -> Option<crate::ime::KeyboardEdit> {
        None
    }
    /// The session to show next ([`Reopen`]); `None` when the platform
    /// can't.
    fn reopen(&self, _how: Reopen) -> Option<crate::interact::Session> {
        None
    }
}

/// Something the platform told the app (Android's callbacks, through the
/// bridge).
#[derive(Debug, Clone, PartialEq)]
pub enum PlatformEvent {
    /// A link opened the app (`construct://…`).
    Link(String),
    /// Photos came back from the picker, each copied to a local file (or
    /// why it couldn't be read); empty when it was closed. The app stages
    /// them (`Session::picked`).
    Picked(Vec<Result<std::path::PathBuf, String>>),
    /// A message arrived for the app.
    Message(String),
}

/// Why the window wants another session.
#[derive(Debug, Clone, PartialEq)]
pub enum Reopen {
    /// Signed in: save the session and open the pod; `code` is the one to
    /// offer next time.
    SignedIn { token: String, code: String },
    /// Left the pod: forget the session, show the pod's sign-in screen with
    /// the last code.
    Left,
    /// Load the pod again (`refresh`, Try again).
    Reload,
    /// Open another server (`set_server`): its saved session, else its
    /// sign-in screen.
    Server(String),
}

/// The desktop's clipboard, through its command-line tools (`pbpaste` on
/// macOS, `wl-paste`, `xclip` or `xsel` on Linux).
pub fn clipboard_here() -> Option<String> {
    let tries: &[&[&str]] = if cfg!(target_os = "macos") {
        &[&["pbpaste"]]
    } else {
        &[
            &["wl-paste", "--no-newline"],
            &["xclip", "-selection", "clipboard", "-o"],
            &["xsel", "--clipboard", "--output"],
        ]
    };
    tries.iter().find_map(|cmd| {
        let out = std::process::Command::new(cmd[0])
            .args(&cmd[1..])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    })
}

/// Puts text on the desktop's clipboard (`pbcopy`, `wl-copy`, `xclip`, `xsel`).
pub fn set_clipboard_here(text: &str) {
    use std::io::Write;
    let tries: &[&[&str]] = if cfg!(target_os = "macos") {
        &[&["pbcopy"]]
    } else {
        &[
            &["wl-copy"],
            &["xclip", "-selection", "clipboard"],
            &["xsel", "--clipboard", "--input"],
        ]
    };
    for cmd in tries {
        let Ok(mut child) = std::process::Command::new(cmd[0])
            .args(&cmd[1..])
            .stdin(std::process::Stdio::piped())
            .spawn()
        else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        if child.wait().is_ok_and(|s| s.success()) {
            return;
        }
    }
    eprintln!("construct-render: no clipboard tool (pbcopy, wl-copy, xclip or xsel) took the text");
}

/// Opens an address with the desktop's opener (`open`, `xdg-open`, `start`).
pub fn open_link_here(url: &str) {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    if let Err(e) = cmd.arg(url).spawn() {
        eprintln!("construct-render: can't open {url}: {e}");
    }
}
