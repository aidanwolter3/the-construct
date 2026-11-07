//! The Rust side of the Android bridge (`android/java`): the few Android
//! callbacks a NativeActivity doesn't pass on, forwarded by small Java
//! classes that hold no app logic. A link that opens the app and the photo
//! picker's result (read by `crate::picked`) come from `ConstructActivity`; a message for the app,
//! also while it is closed (what a push does), from `ConstructReceiver`.
//! Events queue here and wake the window, which reads them through
//! `Platform::events`.

use std::sync::{Mutex, OnceLock};

use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JClass, JObject, JObjectArray, JString, JValue};
use jni::{EnvUnowned, jni_sig, jni_str};

use crate::window::PlatformEvent;

static EVENTS: Mutex<Vec<PlatformEvent>> = Mutex::new(Vec::new());
static WAKER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

fn log(m: &str) {
    crate::android::log(m);
}

/// Wakes the window when an event arrives (set once the event loop exists).
pub fn set_waker(wake: impl Fn() + Send + Sync + 'static) {
    let _ = WAKER.set(Box::new(wake));
}

/// The events since last asked.
pub fn take_events() -> Vec<PlatformEvent> {
    std::mem::take(&mut *EVENTS.lock().unwrap_or_else(|p| p.into_inner()))
}

fn push(e: PlatformEvent) {
    EVENTS.lock().unwrap_or_else(|p| p.into_inner()).push(e);
    wake();
}

/// Wakes the window (the keyboard's input connection, `ime.rs`, edited the
/// field).
pub(crate) fn wake() {
    if let Some(w) = WAKER.get() {
        w();
    }
}

/// `Native.link(uri)`: a link opened the app.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_theconstruct_android_Native_link<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    uri: JString<'l>,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        push(PlatformEvent::Link(uri.try_to_string(env)?));
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.picked(context, uris)`: the photo picker's result, read into
/// local files (`crate::picked`) for the media picker to stage.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_theconstruct_android_Native_picked<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    context: JObject<'l>,
    uris: JObjectArray<'l>,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        push(PlatformEvent::Picked(crate::picked::read_all(
            env, &context, &uris,
        )?));
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.message(context, message)`: a message for the app. It shows as a
/// notification (the app may be closed: only the receiver runs), and in the
/// app when it is open.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_theconstruct_android_Native_message<'l>(
    mut env: EnvUnowned<'l>,
    _class: JClass<'l>,
    context: JObject<'l>,
    message: JString<'l>,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let text = if message.is_null() {
            "(no message)".to_owned()
        } else {
            message.try_to_string(env)?
        };
        log("message received");
        if let Err(e) = notify(env, &context, &text) {
            log(&format!("notification: {e}"));
            env.exception_clear();
        }
        push(PlatformEvent::Message(text));
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// Posts a notification that opens the app when tapped.
fn notify(env: &mut jni::Env, context: &JObject, text: &str) -> jni::errors::Result<()> {
    let service = env.new_string("notification")?;
    let nm = env
        .call_method(
            context,
            jni_str!("getSystemService"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
            &[JValue::Object(&service)],
        )?
        .l()?;
    let id = env.new_string("messages")?;
    let name = env.new_string("Messages")?;
    let channel = env.new_object(
        jni_str!("android/app/NotificationChannel"),
        jni_sig!("(Ljava/lang/String;Ljava/lang/CharSequence;I)V"),
        &[JValue::Object(&id), JValue::Object(&name), JValue::Int(3)],
    )?;
    env.call_method(
        &nm,
        jni_str!("createNotificationChannel"),
        jni_sig!("(Landroid/app/NotificationChannel;)V"),
        &[JValue::Object(&channel)],
    )?;
    let builder = env.new_object(
        jni_str!("android/app/Notification$Builder"),
        jni_sig!("(Landroid/content/Context;Ljava/lang/String;)V"),
        &[JValue::Object(context), JValue::Object(&id)],
    )?;
    let icon = env
        .get_static_field(
            jni_str!("android/R$drawable"),
            jni_str!("ic_dialog_info"),
            jni_sig!("I"),
        )?
        .i()?;
    let title = env.new_string("The Construct")?;
    let body = env.new_string(text)?;
    let set = |env: &mut jni::Env, name, sig, arg: JValue| {
        env.call_method(&builder, name, sig, &[arg]).map(|_| ())
    };
    set(
        env,
        jni_str!("setSmallIcon"),
        jni_sig!("(I)Landroid/app/Notification$Builder;"),
        JValue::Int(icon),
    )?;
    set(
        env,
        jni_str!("setContentTitle"),
        jni_sig!("(Ljava/lang/CharSequence;)Landroid/app/Notification$Builder;"),
        JValue::Object(&title),
    )?;
    set(
        env,
        jni_str!("setContentText"),
        jni_sig!("(Ljava/lang/CharSequence;)Landroid/app/Notification$Builder;"),
        JValue::Object(&body),
    )?;
    set(
        env,
        jni_str!("setAutoCancel"),
        jni_sig!("(Z)Landroid/app/Notification$Builder;"),
        JValue::Bool(true),
    )?;
    // Tapping it opens the app.
    let pm = env
        .call_method(
            context,
            jni_str!("getPackageManager"),
            jni_sig!("()Landroid/content/pm/PackageManager;"),
            &[],
        )?
        .l()?;
    let pkg = env
        .call_method(
            context,
            jni_str!("getPackageName"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    let launch = env
        .call_method(
            &pm,
            jni_str!("getLaunchIntentForPackage"),
            jni_sig!("(Ljava/lang/String;)Landroid/content/Intent;"),
            &[JValue::Object(&pkg)],
        )?
        .l()?;
    if !launch.is_null() {
        // FLAG_IMMUTABLE | FLAG_UPDATE_CURRENT.
        let pi = env
            .call_static_method(
                jni_str!("android/app/PendingIntent"),
                jni_str!("getActivity"),
                jni_sig!("(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;"),
                &[
                    JValue::Object(context),
                    JValue::Int(0),
                    JValue::Object(&launch),
                    JValue::Int(0x0400_0000 | 0x0800_0000),
                ],
            )?
            .l()?;
        set(
            env,
            jni_str!("setContentIntent"),
            jni_sig!("(Landroid/app/PendingIntent;)Landroid/app/Notification$Builder;"),
            JValue::Object(&pi),
        )?;
    }
    let n = env
        .call_method(
            &builder,
            jni_str!("build"),
            jni_sig!("()Landroid/app/Notification;"),
            &[],
        )?
        .l()?;
    env.call_method(
        &nm,
        jni_str!("notify"),
        jni_sig!("(ILandroid/app/Notification;)V"),
        &[JValue::Int(1), JValue::Object(&n)],
    )?;
    Ok(())
}

/// Opens the photo picker (`ConstructActivity.pickImages`).
pub fn pick_images(env: &mut jni::Env, activity: &JObject, max: u32) -> jni::errors::Result<()> {
    env.call_method(
        activity,
        jni_str!("pickImages"),
        jni_sig!("(I)V"),
        &[JValue::Int(max.min(i32::MAX as u32) as i32)],
    )?;
    Ok(())
}

/// Asks for the notification permission (Android 13+), once.
pub fn ask_notifications(env: &mut jni::Env, activity: &JObject) -> jni::errors::Result<()> {
    let sdk = env
        .get_static_field(
            jni_str!("android/os/Build$VERSION"),
            jni_str!("SDK_INT"),
            jni_sig!("I"),
        )?
        .i()?;
    if sdk < 33 {
        return Ok(());
    }
    let perm = env.new_string("android.permission.POST_NOTIFICATIONS")?;
    let granted = env
        .call_method(
            activity,
            jni_str!("checkSelfPermission"),
            jni_sig!("(Ljava/lang/String;)I"),
            &[JValue::Object(&perm)],
        )?
        .i()?;
    if granted == 0 {
        return Ok(());
    }
    env.call_method(
        activity,
        jni_str!("askPermission"),
        jni_sig!("(Ljava/lang/String;I)V"),
        &[JValue::Object(&perm), JValue::Int(7)],
    )?;
    Ok(())
}
