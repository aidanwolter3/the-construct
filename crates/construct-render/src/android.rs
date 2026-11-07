//! The Android entry point: a NativeActivity whose window the engine draws
//! with Vulkan (or GL), no JVM UI in between. Build and run it with
//! `scripts/android_native.sh`.
//!
//! With `pod.json` (`{"url", "token"}`, a session token) in its internal
//! files directory, it opens that pod's home live (`crate::remote`):
//! `scripts/android_native.sh --pod` puts it there. Otherwise the activity
//! opens `layout.json` from its external files directory
//! (`{"layout", "data"}`, pushed with adb) or the bundled demo. With
//! `frame-request` there too, it first draws that layout offscreen at
//! 390 × 844 @2x on the device's GPU and saves `frame.png` next to it, which
//! the script compares with the desktop goldens.

use std::ffi::CString;
use std::path::PathBuf;
use winit::event_loop::EventLoop;
use winit::platform::android::EventLoopBuilderExtAndroid;
use winit::platform::android::activity::AndroidApp;

use crate::interact::Session;
use crate::viewer::Controller;
use crate::{Viewport, demo};

#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_write(
        prio: i32,
        tag: *const std::ffi::c_char,
        text: *const std::ffi::c_char,
    ) -> i32;
}

/// A line in logcat under the tag `construct-render`.
pub(crate) fn log(line: &str) {
    const INFO: i32 = 4;
    let tag = c"construct-render";
    if let Ok(text) = CString::new(line.replace('\0', " ")) {
        // SAFETY: both pointers are valid NUL-terminated strings for the call.
        unsafe {
            __android_log_write(INFO, tag.as_ptr(), text.as_ptr());
        }
    }
}

fn load(dir: Option<&PathBuf>) -> (String, protocol::UiNode, protocol::DataBundle) {
    let file = dir.map(|d| d.join("layout.json"));
    if let Some(f) = file.filter(|f| f.exists()) {
        match std::fs::read_to_string(&f)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
            .and_then(|v| demo::parse(&v).map_err(|e| e.to_string()))
        {
            Ok(l) => {
                log(&format!("opened {}", f.display()));
                return l;
            }
            Err(e) => log(&format!("{}: {e}; showing the demo", f.display())),
        }
    }
    demo::welcome().expect("the bundled demo parses")
}

/// Draws the layout offscreen at the golden viewport and saves it.
fn frame_for_comparison(dir: &std::path::Path, session: &mut Session) {
    let mut surface = match pollster::block_on(crate::gpu::HeadlessSurface::new(8, 8)) {
        Ok(s) => s,
        Err(e) => return log(&format!("offscreen frame: {e}")),
    };
    let started = std::time::Instant::now();
    session.resize(Viewport::PHONE);
    let result = session
        .scene_and_text()
        .map_err(|e| e.to_string())
        .and_then(|(scene, text)| surface.render(scene, text).map_err(|e| e.to_string()));
    match result {
        Ok(img) => {
            let out = dir.join("frame.png");
            match std::fs::write(&out, crate::encode::png(&img)) {
                Ok(()) => log(&format!(
                    "offscreen frame on {} in {:.2} ms: {}",
                    surface.gpu.describe(),
                    started.elapsed().as_secs_f64() * 1000.0,
                    out.display()
                )),
                Err(e) => log(&format!("offscreen frame: {e}")),
            }
        }
        Err(e) => log(&format!("offscreen frame: {e}")),
    }
}

/// The soft keyboard and the system bars, for the window.
struct Android(AndroidApp);

impl crate::window::Platform for Android {
    fn show_keyboard(&self, show: bool) {
        if show {
            self.0.show_soft_input(true);
        } else {
            self.0.hide_soft_input(false);
        }
    }

    fn insets_px(&self, (_w, h): (u32, u32)) -> [f32; 4] {
        // The window already sits below the status bar and cutout (the app
        // targets SDK 34); what can still cover it is the keyboard, which
        // shrinks the content rect but not the window. Only its height is
        // used, whatever origin the rect is measured from.
        let r = self.0.content_rect();
        let visible = r.bottom - r.top;
        if visible <= 0 {
            return [0.0; 4];
        }
        [0.0, 0.0, (h as i32 - visible).max(0) as f32, 0.0]
    }
}

/// A page saying what went wrong, in place of the pod's home.
fn error_page(message: &str) -> Session {
    let root: protocol::UiNode = serde_json::from_value(serde_json::json!({
        "type": "core://Page", "id": "error", "root": { "type": "core://Column",
        "padding_h_dp": 16, "padding_v_dp": 16, "spacing_dp": 8, "children": [
            { "type": "core://Text", "value_expr": "Couldn't open the pod", "style": "Title" },
            { "type": "core://Text", "value_expr": message, "style": "Body" },
            { "type": "core://Text", "value_expr": "Run scripts/android_native.sh --pod again, then reopen the app.", "style": "Secondary" }
        ] } }))
    .expect("a valid page");
    Session::new(
        "error",
        root,
        protocol::DataBundle::default(),
        Viewport::PHONE,
    )
}

/// The pod's home, live, when `pod.json` is in the internal files dir.
fn pod_session(app: &AndroidApp) -> Option<Session> {
    let file = app.internal_data_path()?.join("pod.json");
    let text = std::fs::read_to_string(&file).ok()?;
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return Some(error_page(&format!("pod.json: {e}"))),
    };
    let (Some(url), Some(token)) = (v["url"].as_str(), v["token"].as_str()) else {
        return Some(error_page("pod.json needs url and token"));
    };
    log(&format!("opening {url}"));
    let started = std::time::Instant::now();
    let session = crate::remote::PodClient::connect(url, token)
        .and_then(|c| c.session("home", Viewport::PHONE));
    Some(match session {
        Ok(s) => {
            log(&format!(
                "home loaded in {:.0} ms",
                started.elapsed().as_secs_f64() * 1000.0
            ));
            s
        }
        Err(e) => {
            log(&format!("couldn't open the pod: {e}"));
            error_page(&e)
        }
    })
}

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    log("construct-render starting");
    let session = match pod_session(&app) {
        Some(s) => s,
        None => {
            let dir = app.external_data_path();
            let (id, root, data) = load(dir.as_ref());
            let mut session = Session::new(&id, root, data, Viewport::PHONE);
            if let Some(d) = dir.as_ref().filter(|d| d.join("frame-request").exists()) {
                frame_for_comparison(d, &mut session);
            }
            session
        }
    };
    let event_loop = match EventLoop::builder().with_android_app(app.clone()).build() {
        Ok(l) => l,
        Err(e) => return log(&format!("no event loop: {e}")),
    };
    let window = crate::window::App::new(Controller::new(session), "The Construct")
        .with_platform(Box::new(Android(app)));
    if let Err(e) = crate::window::run_app(event_loop, window) {
        log(&format!("window: {e}"));
    }
}
