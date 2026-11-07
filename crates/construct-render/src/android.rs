//! The Android entry point: a NativeActivity whose window the engine draws
//! with Vulkan (or GL), no JVM UI in between. Build and run it with
//! `scripts/android_native.sh`.
//!
//! It is installed as the main app's package, so it opens a pod's home live
//! (`crate::remote`) with `pod.json` (`{"url", "token"}`, a session token)
//! from its internal files directory, which the script saves, or else the
//! main app's own saved sign-in. Without either, it opens `layout.json` from
//! its external files directory (`{"layout", "data"}`, pushed with adb) or
//! the bundled demo. With `frame-request` there too, it first draws that
//! layout offscreen at 390 × 844 @2x on the device's GPU and saves
//! `frame.png` next to it (`frame_diff` compares it with the goldens).

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

    fn gone(&self) -> bool {
        activity_gone(&self.0)
    }

    fn dark(&self) -> Option<bool> {
        night_mode(&self.0)
    }

    fn clipboard(&self) -> Option<String> {
        clipboard_text(&self.0)
    }

    fn set_clipboard(&self, text: &str) {
        if let Err(e) = set_clipboard_text(&self.0, text) {
            log(&format!("clipboard: {e}"));
        }
    }

    fn reopen(&self, how: crate::window::Reopen) -> Option<Session> {
        Some(pod_home(&self.0).reopen(how))
    }

    fn open_link(&self, url: &str) {
        if let Err(e) = view_url(&self.0, url) {
            log(&format!("can't open {url}: {e}"));
        }
    }
}

/// Whether the activity is finishing or destroyed (`isFinishing`,
/// `isDestroyed`): Android then waits for `android_main` to return.
fn activity_gone(app: &AndroidApp) -> bool {
    use jni::objects::JObject;
    use jni::{jni_sig, jni_str};
    // SAFETY: the VM pointer is valid for the life of the process.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let activity = app.activity_as_ptr() as jni::sys::jobject;
    vm.attach_current_thread(|env| -> jni::errors::Result<bool> {
        let run = |env: &mut jni::Env| -> jni::errors::Result<bool> {
            // SAFETY: android-activity keeps its own reference to the activity.
            let activity = unsafe { env.as_cast_raw::<JObject>(&activity)? };
            let mut gone = false;
            for m in [jni_str!("isFinishing"), jni_str!("isDestroyed")] {
                gone |= env.call_method(&*activity, m, jni_sig!("()Z"), &[])?.z()?;
            }
            Ok(gone)
        };
        Ok(run(env).unwrap_or_else(|e| {
            log(&format!("activity state: {}", java_error(env, e)));
            false
        }))
    })
    .unwrap_or(false)
}

/// Whether the system is in dark mode: the activity's
/// `getResources().getConfiguration().uiMode` has `UI_MODE_NIGHT_YES`. The
/// activity takes `uiMode` changes itself (the manifest), so this is read
/// again after each batch of events.
fn night_mode(app: &AndroidApp) -> Option<bool> {
    use jni::objects::JObject;
    use jni::{jni_sig, jni_str};
    const UI_MODE_NIGHT_MASK: i32 = 0x30;
    const UI_MODE_NIGHT_YES: i32 = 0x20;
    // SAFETY: the VM pointer is valid for the life of the process.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let activity = app.activity_as_ptr() as jni::sys::jobject;
    vm.attach_current_thread(|env| -> jni::errors::Result<Option<bool>> {
        let run = |env: &mut jni::Env| -> jni::errors::Result<bool> {
            // SAFETY: android-activity keeps its own reference to the activity.
            let activity = unsafe { env.as_cast_raw::<JObject>(&activity)? };
            let res = env
                .call_method(
                    &*activity,
                    jni_str!("getResources"),
                    jni_sig!("()Landroid/content/res/Resources;"),
                    &[],
                )?
                .l()?;
            let config = env
                .call_method(
                    &res,
                    jni_str!("getConfiguration"),
                    jni_sig!("()Landroid/content/res/Configuration;"),
                    &[],
                )?
                .l()?;
            let mode = env
                .get_field(&config, jni_str!("uiMode"), jni_sig!("I"))?
                .i()?;
            Ok(mode & UI_MODE_NIGHT_MASK == UI_MODE_NIGHT_YES)
        };
        Ok(match run(env) {
            Ok(d) => Some(d),
            Err(e) => {
                log(&format!("night mode: {}", java_error(env, e)));
                None
            }
        })
    })
    .ok()
    .flatten()
}

/// The clipboard's text (`ClipboardManager.getPrimaryClip()`'s first item,
/// as text). Android lets the app in front read it.
fn clipboard_text(app: &AndroidApp) -> Option<String> {
    use jni::objects::{JObject, JString, JValue};
    use jni::{jni_sig, jni_str};
    // SAFETY: the VM pointer is valid for the life of the process.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let activity = app.activity_as_ptr() as jni::sys::jobject;
    vm.attach_current_thread(|env| -> jni::errors::Result<Option<String>> {
        let run = |env: &mut jni::Env| -> jni::errors::Result<Option<String>> {
            // SAFETY: android-activity keeps its own reference to the activity.
            let activity = unsafe { env.as_cast_raw::<JObject>(&activity)? };
            let name = env.new_string("clipboard")?;
            let cm = env
                .call_method(
                    &*activity,
                    jni_str!("getSystemService"),
                    jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                    &[JValue::Object(&name)],
                )?
                .l()?;
            if cm.is_null() {
                return Ok(None);
            }
            let clip = env
                .call_method(
                    &cm,
                    jni_str!("getPrimaryClip"),
                    jni_sig!("()Landroid/content/ClipData;"),
                    &[],
                )?
                .l()?;
            if clip.is_null() {
                return Ok(None);
            }
            let count = env
                .call_method(&clip, jni_str!("getItemCount"), jni_sig!("()I"), &[])?
                .i()?;
            if count < 1 {
                return Ok(None);
            }
            let item = env
                .call_method(
                    &clip,
                    jni_str!("getItemAt"),
                    jni_sig!("(I)Landroid/content/ClipData$Item;"),
                    &[JValue::Int(0)],
                )?
                .l()?;
            let text = env
                .call_method(
                    &item,
                    jni_str!("coerceToText"),
                    jni_sig!("(Landroid/content/Context;)Ljava/lang/CharSequence;"),
                    &[JValue::Object(&activity)],
                )?
                .l()?;
            if text.is_null() {
                return Ok(None);
            }
            let s = env
                .call_method(
                    &text,
                    jni_str!("toString"),
                    jni_sig!("()Ljava/lang/String;"),
                    &[],
                )?
                .l()?;
            let s = env.cast_local::<JString>(s)?;
            Ok(Some(s.try_to_string(env)?))
        };
        Ok(run(env).unwrap_or_else(|e| {
            log(&format!("clipboard: {}", java_error(env, e)));
            None
        }))
    })
    .ok()
    .flatten()
    .filter(|t| !t.is_empty())
}

/// Puts text on the clipboard (`ClipData.newPlainText`, `setPrimaryClip`).
fn set_clipboard_text(app: &AndroidApp, text: &str) -> Result<(), String> {
    use jni::objects::{JObject, JValue};
    use jni::{jni_sig, jni_str};
    // SAFETY: the VM pointer is valid for the life of the process.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let activity = app.activity_as_ptr() as jni::sys::jobject;
    vm.attach_current_thread(|env| -> jni::errors::Result<Result<(), String>> {
        let run = |env: &mut jni::Env| -> jni::errors::Result<()> {
            // SAFETY: android-activity keeps its own reference to the activity.
            let activity = unsafe { env.as_cast_raw::<JObject>(&activity)? };
            let name = env.new_string("clipboard")?;
            let cm = env
                .call_method(
                    &*activity,
                    jni_str!("getSystemService"),
                    jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                    &[JValue::Object(&name)],
                )?
                .l()?;
            let label = env.new_string("The Construct")?;
            let value = env.new_string(text)?;
            let clip = env
                .call_static_method(
                    jni_str!("android/content/ClipData"),
                    jni_str!("newPlainText"),
                    jni_sig!("(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;"),
                    &[JValue::Object(&label), JValue::Object(&value)],
                )?
                .l()?;
            env.call_method(
                &cm,
                jni_str!("setPrimaryClip"),
                jni_sig!("(Landroid/content/ClipData;)V"),
                &[JValue::Object(&clip)],
            )?;
            Ok(())
        };
        Ok(run(env).map_err(|e| java_error(env, e)))
    })
    .map_err(|e| e.to_string())?
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

use crate::pods::DEFAULT_SERVER;

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

/// The server the main app uses (`shared_prefs/server.xml`), else staging.
fn main_app_server(app: &AndroidApp) -> String {
    app.internal_data_path()
        .and_then(|f| {
            let x = std::fs::read_to_string(f.parent()?.join("shared_prefs/server.xml")).ok()?;
            pref(&x, "baseUrl")
        })
        .unwrap_or_else(|| DEFAULT_SERVER.to_owned())
        .trim_end_matches('/')
        .to_owned()
}

/// A pending Java exception, cleared and described (Java keeps it pending
/// after a failed call, and the next JNI call would abort the app).
fn java_error(env: &mut jni::Env, e: jni::errors::Error) -> String {
    use jni::objects::JString;
    use jni::{jni_sig, jni_str};
    if !env.exception_check() {
        return e.to_string();
    }
    let Some(t) = env.exception_occurred() else {
        return e.to_string();
    };
    env.exception_clear();
    let text = env
        .call_method(
            &t,
            jni_str!("toString"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )
        .and_then(|v| v.l())
        .and_then(|o| env.cast_local::<JString>(o))
        .and_then(|s| s.try_to_string(env));
    env.exception_clear();
    text.unwrap_or_else(|_| e.to_string())
}

/// The session the main app (installed as the same package, see
/// `scripts/android_native.sh`) saved: its server (`server.xml`,
/// else staging) and its pod session, AES-GCM encrypted with the key
/// `construct_session_key` in the Android keystore (`KeystoreSecureStore`).
/// `Ok(None)`: this isn't the main app's package; `Err`: why its session
/// can't be used.
fn main_app_session(app: &AndroidApp) -> Result<Option<(String, String)>, String> {
    let files = app.internal_data_path().ok_or("no data directory")?;
    if !files.to_string_lossy().contains("org.theconstruct.android") {
        return Ok(None);
    }
    let prefs = files
        .parent()
        .ok_or("no data directory")?
        .join("shared_prefs");
    let server = std::fs::read_to_string(prefs.join("server.xml"))
        .ok()
        .and_then(|x| pref(&x, "baseUrl"))
        .unwrap_or_else(|| DEFAULT_SERVER.to_owned());
    let server = server.trim_end_matches('/').to_owned();
    log(&format!("the main app's server: {server}"));
    let secure = std::fs::read_to_string(prefs.join("construct_secure.xml")).map_err(|e| {
        format!(
            "The Construct has no saved sign-in on this phone ({}: {e}). Showing the pod's sign-in screen.",
            prefs.join("construct_secure.xml").display()
        )
    })?;
    let key = format!("pod_session:{server}");
    let Some(stored) = pref(&secure, &key) else {
        // Names only (never values), to see which servers have a session.
        let names: Vec<&str> = secure
            .split("<string name=\"")
            .skip(1)
            .filter_map(|r| r.split('"').next())
            .collect();
        return Err(format!(
            "No saved session for {server} (saved: {names:?}). Showing the pod's sign-in \
             screen."
        ));
    };
    let (iv, ct) = stored
        .split_once(':')
        .ok_or("the saved session isn't in the expected form")?;
    let raw = keystore_decrypt(app, iv, ct)
        .map_err(|e| format!("The saved session doesn't decrypt: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("the saved session: {e}"))?;
    let token = v["token"]
        .as_str()
        .ok_or("the saved session has no token")?
        .to_owned();
    log(&format!("using the main app's session for {server}"));
    Ok(Some((server, token)))
}

/// `KeystoreSecureStore.get`'s decryption, through the Android SDK: base64
/// IV and ciphertext, AES/GCM/NoPadding with a 128-bit tag.
fn keystore_decrypt(app: &AndroidApp, iv_b64: &str, ct_b64: &str) -> Result<String, String> {
    use jni::objects::{JByteArray, JObject, JValue};
    use jni::{jni_sig, jni_str};
    // SAFETY: the VM pointer is valid for the life of the process.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let bytes = vm
        .attach_current_thread(|env| -> jni::errors::Result<Result<Vec<u8>, String>> {
            let run = |env: &mut jni::Env| -> jni::errors::Result<Vec<u8>> {
                let iv_s = env.new_string(iv_b64)?;
                let iv = env
                    .call_static_method(
                        jni_str!("android/util/Base64"),
                        jni_str!("decode"),
                        jni_sig!("(Ljava/lang/String;I)[B"),
                        &[JValue::Object(&iv_s), JValue::Int(2)],
                    )?
                    .l()?;
                let ct_s = env.new_string(ct_b64)?;
                let ct = env
                    .call_static_method(
                        jni_str!("android/util/Base64"),
                        jni_str!("decode"),
                        jni_sig!("(Ljava/lang/String;I)[B"),
                        &[JValue::Object(&ct_s), JValue::Int(2)],
                    )?
                    .l()?;
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
                if key.is_null() {
                    return Err(jni::errors::Error::NullPtr(
                        "no construct_session_key in the keystore",
                    ));
                }
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
            };
            Ok(run(env).map_err(|e| java_error(env, e)))
        })
        .map_err(|e| e.to_string())??;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

/// This app's pod: kept in its private files (`pod.json`), else the main
/// app's own sign-in, else the sign-in screen of the main app's server.
fn pod_home(app: &AndroidApp) -> crate::pods::PodHome {
    let server = main_app_server(app);
    crate::pods::PodHome {
        file: app.internal_data_path().map(|d| d.join("pod.json")),
        // As the main app's picker (`ServerPrefs`): staging, the emulator's
        // host, USB (adb reverse), and the main app's server when it is
        // another.
        servers: crate::pods::server_options(
            &[
                ("emulator", "Local emulator", "http://10.0.2.2:8080"),
                ("usb", "Local USB", "http://127.0.0.1:8080"),
            ],
            &server,
        ),
        server,
        log,
    }
}

fn pod_session(app: &AndroidApp) -> Session {
    pod_home(app).start(|| match main_app_session(app) {
        Ok(found) => found,
        Err(e) => {
            log(&e);
            None
        }
    })
}

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    log("construct-render starting");
    // A layout pushed to the external files dir (development) wins; else
    // the pod.
    let dir = app.external_data_path();
    let session = if dir.as_ref().is_some_and(|d| d.join("layout.json").exists()) {
        let (id, root, data) = load(dir.as_ref());
        let mut session = Session::new(&id, root, data, Viewport::PHONE);
        if let Some(d) = dir.as_ref().filter(|d| d.join("frame-request").exists()) {
            frame_for_comparison(d, &mut session);
        }
        session
    } else {
        pod_session(&app)
    };
    // winit makes one event loop per process, but Android keeps the process
    // when the activity goes (Back, a swipe from recents) and calls
    // `android_main` again on the next launch. So the process ends with its
    // activity, and every launch starts afresh.
    let event_loop = match EventLoop::builder().with_android_app(app.clone()).build() {
        Ok(l) => l,
        Err(e) => {
            log(&format!("no event loop: {e}; restarting"));
            std::process::exit(0);
        }
    };
    let window = crate::window::App::new(Controller::new(session), "The Construct")
        .with_platform(Box::new(Android(app, Default::default())));
    if let Err(e) = crate::window::run_app(event_loop, window) {
        log(&format!("window: {e}"));
    }
    log("the activity is gone: ending the process");
    std::process::exit(0);
}
