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
struct Android(AndroidApp, std::sync::Mutex<[f32; 4]>);

impl crate::window::Platform for Android {
    fn show_keyboard(&self, show: bool) {
        if show {
            self.0.show_soft_input(true);
        } else {
            self.0.hide_soft_input(false);
        }
    }

    fn insets_px(&self, (_w, h): (u32, u32)) -> [f32; 4] {
        // What the system covers of this window, as Android itself reports
        // it: system bars, the camera cutout and the keyboard. Only when that
        // can't be read, the content rect (which leaves out the status bar
        // on some devices).
        let insets = match window_insets(&self.0) {
            Some([l, t, r, b]) => [t as f32, r as f32, b as f32, l as f32],
            None => {
                let r = self.0.content_rect();
                let visible = r.bottom - r.top;
                if visible <= 0 {
                    [0.0; 4]
                } else {
                    [0.0, 0.0, (h as i32 - visible).max(0) as f32, 0.0]
                }
            }
        };
        let mut last = self.1.lock().unwrap_or_else(|p| p.into_inner());
        if *last != insets {
            log(&format!("insets (px, top right bottom left): {insets:?}"));
            *last = insets;
        }
        insets
    }

    fn open_link(&self, url: &str) {
        if let Err(e) = view_url(&self.0, url) {
            log(&format!("can't open {url}: {e}"));
        }
    }
}

/// Opens `url` in the browser: an `ACTION_VIEW` intent from the activity.
fn view_url(app: &AndroidApp, url: &str) -> jni::errors::Result<()> {
    use jni::objects::{JObject, JValue};
    use jni::{jni_sig, jni_str};
    // SAFETY: the VM pointer is valid for the life of the process.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let activity = app.activity_as_ptr() as jni::sys::jobject;
    vm.attach_current_thread(|env| -> jni::errors::Result<()> {
        // SAFETY: the activity reference is valid while the app runs.
        let activity = unsafe { env.as_cast_raw::<JObject>(&activity)? };
        let s = env.new_string(url)?;
        let uri = env
            .call_static_method(
                jni_str!("android/net/Uri"),
                jni_str!("parse"),
                jni_sig!("(Ljava/lang/String;)Landroid/net/Uri;"),
                &[JValue::Object(&s)],
            )?
            .l()?;
        let action = env.new_string("android.intent.action.VIEW")?;
        let intent = env.new_object(
            jni_str!("android/content/Intent"),
            jni_sig!("(Ljava/lang/String;Landroid/net/Uri;)V"),
            &[JValue::Object(&action), JValue::Object(&uri)],
        )?;
        env.call_method(
            &*activity,
            jni_str!("startActivity"),
            jni_sig!("(Landroid/content/Intent;)V"),
            &[JValue::Object(&intent)],
        )?;
        Ok(())
    })
}

/// The window's insets in pixels (left, top, right, bottom):
/// `getRootWindowInsets()` of the decor view, as system bars, cutout and
/// keyboard (`WindowInsets.getInsets` on Android 11+, the system window insets
/// before). `None` when it can't be read (no view attached yet).
fn window_insets(app: &AndroidApp) -> Option<[i32; 4]> {
    use jni::objects::{JObject, JValue};
    use jni::{jni_sig, jni_str};
    // SAFETY: the VM pointer is valid for the life of the process.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let activity = app.activity_as_ptr() as jni::sys::jobject;
    vm.attach_current_thread(|env| -> jni::errors::Result<Option<[i32; 4]>> {
        // SAFETY: the activity reference is valid while the app runs.
        let activity = unsafe { env.as_cast_raw::<JObject>(&activity)? };
        let window = env
            .call_method(
                &*activity,
                jni_str!("getWindow"),
                jni_sig!("()Landroid/view/Window;"),
                &[],
            )?
            .l()?;
        let decor = env
            .call_method(
                &window,
                jni_str!("getDecorView"),
                jni_sig!("()Landroid/view/View;"),
                &[],
            )?
            .l()?;
        let insets = env
            .call_method(
                &decor,
                jni_str!("getRootWindowInsets"),
                jni_sig!("()Landroid/view/WindowInsets;"),
                &[],
            )?
            .l()?;
        if insets.is_null() {
            return Ok(None);
        }
        let sdk = env
            .get_static_field(
                jni_str!("android/os/Build$VERSION"),
                jni_str!("SDK_INT"),
                jni_sig!("I"),
            )?
            .i()?;
        if sdk >= 30 {
            let types = jni_str!("android/view/WindowInsets$Type");
            let mut mask = 0;
            for t in [
                jni_str!("systemBars"),
                jni_str!("displayCutout"),
                jni_str!("ime"),
            ] {
                mask |= env
                    .call_static_method(types, t, jni_sig!("()I"), &[])?
                    .i()?;
            }
            let i = env
                .call_method(
                    &insets,
                    jni_str!("getInsets"),
                    jni_sig!("(I)Landroid/graphics/Insets;"),
                    &[JValue::Int(mask)],
                )?
                .l()?;
            let mut out = [0; 4];
            for (k, f) in [
                jni_str!("left"),
                jni_str!("top"),
                jni_str!("right"),
                jni_str!("bottom"),
            ]
            .into_iter()
            .enumerate()
            {
                out[k] = env.get_field(&i, f, jni_sig!("I"))?.i()?;
            }
            Ok(Some(out))
        } else {
            let mut out = [0; 4];
            for (k, m) in [
                jni_str!("getSystemWindowInsetLeft"),
                jni_str!("getSystemWindowInsetTop"),
                jni_str!("getSystemWindowInsetRight"),
                jni_str!("getSystemWindowInsetBottom"),
            ]
            .into_iter()
            .enumerate()
            {
                out[k] = env.call_method(&insets, m, jni_sig!("()I"), &[])?.i()?;
            }
            Ok(Some(out))
        }
    })
    .unwrap_or_else(|e| {
        log(&format!("window insets: {e:?}"));
        None
    })
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
/// The staging pod: the main app's default server.
const DEFAULT_SERVER: &str = "https://the-construct-pod.fly.dev";

/// A `<string name="key">value</string>` of a SharedPreferences file.
fn pref(xml: &str, key: &str) -> Option<String> {
    let esc = key
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let open = format!("<string name=\"{esc}\">");
    let start = xml.find(&open)? + open.len();
    let len = xml[start..].find("</string>")?;
    Some(
        xml[start..start + len]
            .replace("&quot;", "\"")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

/// The session the main app (installed as the same package, see
/// `scripts/android_native.sh --as-app`) saved: its server (`server.xml`,
/// else staging) and its pod session, AES-GCM encrypted with the key
/// `construct_session_key` in the Android keystore (`KeystoreSecureStore`).
fn main_app_session(app: &AndroidApp) -> Option<(String, String)> {
    let prefs = app.internal_data_path()?.parent()?.join("shared_prefs");
    let server = std::fs::read_to_string(prefs.join("server.xml"))
        .ok()
        .and_then(|x| pref(&x, "baseUrl"))
        .unwrap_or_else(|| DEFAULT_SERVER.to_owned());
    let server = server.trim_end_matches('/').to_owned();
    let secure = std::fs::read_to_string(prefs.join("construct_secure.xml")).ok()?;
    let stored = pref(&secure, &format!("pod_session:{server}"))?;
    let (iv, ct) = stored.split_once(':')?;
    let raw = match keystore_decrypt(app, iv, ct) {
        Ok(r) => r,
        Err(e) => {
            log(&format!("the main app's session doesn't decrypt: {e}"));
            return None;
        }
    };
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let token = v["token"].as_str()?.to_owned();
    log(&format!("using the main app's session for {server}"));
    Some((server, token))
}

/// `KeystoreSecureStore.get`'s decryption, through the Android SDK: base64
/// IV and ciphertext, AES/GCM/NoPadding with a 128-bit tag.
fn keystore_decrypt(app: &AndroidApp, iv_b64: &str, ct_b64: &str) -> Result<String, String> {
    use jni::objects::{JByteArray, JObject, JValue};
    use jni::{jni_sig, jni_str};
    // SAFETY: the VM pointer is valid for the life of the process.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    vm.attach_current_thread(|env| -> jni::errors::Result<Vec<u8>> {
        let mut decode = |s: &str| -> jni::errors::Result<JObject<'_>> {
            let js = env.new_string(s)?;
            env.call_static_method(
                jni_str!("android/util/Base64"),
                jni_str!("decode"),
                jni_sig!("(Ljava/lang/String;I)[B"),
                &[JValue::Object(&js), JValue::Int(2)],
            )?
            .l()
        };
        let iv = decode(iv_b64)?;
        let ct = decode(ct_b64)?;
        let kind = env.new_string("AndroidKeyStore")?;
        let ks = env
            .call_static_method(
                jni_str!("java/security/KeyStore"),
                jni_str!("getInstance"),
                jni_sig!("(Ljava/lang/String;)Ljava/security/KeyStore;"),
                &[JValue::Object(&kind)],
            )?
            .l()?;
        env.call_method(
            &ks,
            jni_str!("load"),
            jni_sig!("(Ljava/security/KeyStore$LoadParameter;)V"),
            &[JValue::Object(&JObject::null())],
        )?;
        let alias = env.new_string("construct_session_key")?;
        let key = env
            .call_method(
                &ks,
                jni_str!("getKey"),
                jni_sig!("(Ljava/lang/String;[C)Ljava/security/Key;"),
                &[JValue::Object(&alias), JValue::Object(&JObject::null())],
            )?
            .l()?;
        let transform = env.new_string("AES/GCM/NoPadding")?;
        let cipher = env
            .call_static_method(
                jni_str!("javax/crypto/Cipher"),
                jni_str!("getInstance"),
                jni_sig!("(Ljava/lang/String;)Ljavax/crypto/Cipher;"),
                &[JValue::Object(&transform)],
            )?
            .l()?;
        let spec = env.new_object(
            jni_str!("javax/crypto/spec/GCMParameterSpec"),
            jni_sig!("(I[B)V"),
            &[JValue::Int(128), JValue::Object(&iv)],
        )?;
        env.call_method(
            &cipher,
            jni_str!("init"),
            jni_sig!("(ILjava/security/Key;Ljava/security/spec/AlgorithmParameterSpec;)V"),
            &[JValue::Int(2), JValue::Object(&key), JValue::Object(&spec)],
        )?;
        let out = env
            .call_method(
                &cipher,
                jni_str!("doFinal"),
                jni_sig!("([B)[B"),
                &[JValue::Object(&ct)],
            )?
            .l()?;
        // SAFETY: doFinal answers a byte[].
        let out = unsafe { JByteArray::from_raw(env, out.into_raw()) };
        env.convert_byte_array(&out)
    })
    .map_err(|e| e.to_string())
    .and_then(|b| String::from_utf8(b).map_err(|e| e.to_string()))
}

fn pod_session(app: &AndroidApp) -> Option<Session> {
    let file = app.internal_data_path()?.join("pod.json");
    let (url, token) = match std::fs::read_to_string(&file) {
        Ok(text) => {
            let v: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => return Some(error_page(&format!("pod.json: {e}"))),
            };
            let (Some(url), Some(token)) = (v["url"].as_str(), v["token"].as_str()) else {
                return Some(error_page("pod.json needs url and token"));
            };
            (url.to_owned(), token.to_owned())
        }
        Err(_) => main_app_session(app)?,
    };
    let (url, token) = (url.as_str(), token.as_str());
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
        .with_platform(Box::new(Android(app, Default::default())));
    if let Err(e) = crate::window::run_app(event_loop, window) {
        log(&format!("window: {e}"));
    }
}
