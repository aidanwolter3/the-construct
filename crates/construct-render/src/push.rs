//! Push notifications while the app is closed (decision
//! `android-push-unifiedpush`): the pod sends Web Push (RFC 8291, D52) to an
//! endpoint a UnifiedPush distributor gives the app: a distributor app
//! (ntfy, ...), else the embedded FCM distributor (Play services, with the
//! pod's VAPID key: an FCM Web Push endpoint, no Firebase project). The
//! distributor's broadcasts reach `PushReceiver` (the connector library's
//! receiver, `android/java`), which only forwards them here; the connector's
//! key store is `PushKeys`, which forwards to this module too.
//!
//! This module keeps one registration per server (decision
//! `android-push-registration-per-server`: instance `server:<base url>`):
//! its Web Push keys, made and kept here, and its endpoint. It decrypts
//! messages, posts them as notifications, and remembers each posted
//! notification under a random token: a tap opens the app with
//! `construct://notification/<token>`, and only a token this module gave
//! out opens anything (the activity is exported, so any app can send it a
//! link). Kept in `push.json` in the app's private files.
//!
//! The platform-free part (keys, decryption, taps) is built with `remote`;
//! the Android part (the receiver's native methods, registration with the
//! distributor, notifications) with the Android window.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use web_push_native::p256;
use web_push_native::p256::elliptic_curve::sec1::ToEncodedPoint;

use crate::engine::PodEngine;

/// Notifications whose taps are remembered (the newest).
const TAPS_KEPT: usize = 50;
/// The link a tapped notification opens the app with, before its token.
pub const TAP_LINK: &str = "construct://notification/";

/// One read-modify-write of `push.json` at a time (the receiver and the
/// window run on different threads).
static FILE: Mutex<()> = Mutex::new(());
/// The engine of the pod the app has open, if it runs: a new endpoint goes
/// to its pod at once, and its messages update its inbox.
static LIVE: Mutex<Option<PodEngine>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The UnifiedPush instance of the pod at `base`: one per server, as the
/// Compose app names them.
pub fn instance_for(base: &str) -> String {
    format!("server:{}", base.trim_end_matches('/'))
}

/// The token of a notification tap link.
pub fn tap_token(uri: &str) -> Option<&str> {
    uri.strip_prefix(TAP_LINK).filter(|t| !t.is_empty())
}

/// A registration's public half, as the pod takes it (base64url).
#[derive(Debug, Clone, PartialEq)]
pub struct Keys {
    pub p256dh: String,
    pub auth: String,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    instances: BTreeMap<String, Registration>,
    #[serde(default)]
    taps: Vec<Tap>,
    /// The notification permission was asked for (`request_notification_permission`).
    #[serde(default)]
    asked: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct Registration {
    /// Base64url of the P-256 private key.
    secret: String,
    /// Base64url of the 16-byte auth secret.
    auth: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    endpoint: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Tap {
    token: String,
    instance: String,
    notification: Value,
}

/// A tapped notification: the server that sent it and the notification.
#[derive(Debug, Clone, PartialEq)]
pub struct Tapped {
    pub instance: String,
    pub notification: Value,
}

fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| e.to_string())?;
    Ok(b)
}

/// The push state, in a file (`push.json`).
pub struct PushStore {
    file: PathBuf,
}

impl PushStore {
    pub fn new(file: PathBuf) -> PushStore {
        PushStore { file }
    }

    fn read(&self) -> State {
        std::fs::read_to_string(&self.file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn write(&self, s: &State) -> Result<(), String> {
        if let Some(d) = self.file.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string(s).map_err(|e| e.to_string())?;
        std::fs::write(&self.file, text).map_err(|e| e.to_string())
    }

    fn change<T>(&self, f: impl FnOnce(&mut State) -> T) -> Result<T, String> {
        let _g = lock(&FILE);
        let mut s = self.read();
        let t = f(&mut s);
        self.write(&s)?;
        Ok(t)
    }

    /// `instance`'s keys, made the first time.
    pub fn keys(&self, instance: &str) -> Result<Keys, String> {
        let _g = lock(&FILE);
        let mut s = self.read();
        let r = match s.instances.get(instance) {
            Some(r) => r.clone(),
            None => {
                let secret = loop {
                    if let Ok(k) = p256::SecretKey::from_slice(&random::<32>()?) {
                        break k;
                    }
                };
                let r = Registration {
                    secret: URL_SAFE_NO_PAD.encode(secret.to_bytes()),
                    auth: URL_SAFE_NO_PAD.encode(random::<16>()?),
                    endpoint: None,
                };
                s.instances.insert(instance.to_owned(), r.clone());
                self.write(&s)?;
                r
            }
        };
        let secret = secret_key(&r)?;
        Ok(Keys {
            p256dh: URL_SAFE_NO_PAD.encode(secret.public_key().to_encoded_point(false).as_bytes()),
            auth: r.auth,
        })
    }

    /// Whether `instance` has keys.
    pub fn exists(&self, instance: &str) -> bool {
        self.read().instances.contains_key(instance)
    }

    /// Forgets `instance`'s keys and endpoint (the connector deletes them
    /// when a registration ends; a new one makes new keys).
    pub fn forget(&self, instance: &str) -> Result<(), String> {
        self.change(|s| {
            s.instances.remove(instance);
        })
    }

    /// The endpoint the distributor gave `instance`.
    pub fn endpoint(&self, instance: &str) -> Option<String> {
        self.read().instances.get(instance)?.endpoint.clone()
    }

    /// Keeps (or, with `None`, forgets) `instance`'s endpoint.
    pub fn set_endpoint(&self, instance: &str, endpoint: Option<&str>) -> Result<(), String> {
        self.change(|s| {
            if let Some(r) = s.instances.get_mut(instance) {
                r.endpoint = endpoint.map(str::to_owned);
            }
        })
    }

    /// A message for `instance`, decrypted: the pod's JSON.
    pub fn decrypt(&self, instance: &str, body: &[u8]) -> Result<String, String> {
        let r = self
            .read()
            .instances
            .get(instance)
            .cloned()
            .ok_or_else(|| format!("no keys for {instance}"))?;
        let auth = URL_SAFE_NO_PAD.decode(&r.auth).map_err(|e| e.to_string())?;
        if auth.len() != 16 {
            return Err("bad auth secret".into());
        }
        let plain = web_push_native::decrypt(
            body.to_vec(),
            &secret_key(&r)?,
            &web_push_native::Auth::clone_from_slice(&auth),
        )
        .map_err(|e| format!("{e:?}"))?;
        String::from_utf8(plain).map_err(|e| e.to_string())
    }

    /// Remembers a posted notification; the token its tap link carries.
    pub fn remember_tap(&self, instance: &str, notification: Value) -> Result<String, String> {
        let token = URL_SAFE_NO_PAD.encode(random::<16>()?);
        self.change(|s| {
            s.taps.push(Tap {
                token: token.clone(),
                instance: instance.to_owned(),
                notification,
            });
            let extra = s.taps.len().saturating_sub(TAPS_KEPT);
            s.taps.drain(..extra);
        })?;
        Ok(token)
    }

    /// The notification a tap link's token was given for, once.
    pub fn take_tap(&self, token: &str) -> Option<Tapped> {
        self.change(|s| {
            let i = s.taps.iter().position(|t| t.token == token)?;
            let t = s.taps.remove(i);
            Some(Tapped {
                instance: t.instance,
                notification: t.notification,
            })
        })
        .ok()
        .flatten()
    }

    /// Whether the notification permission was asked for before; notes
    /// that it now is.
    pub fn asked_before(&self) -> bool {
        self.change(|s| std::mem::replace(&mut s.asked, true))
            .unwrap_or(true)
    }
}

fn secret_key(r: &Registration) -> Result<p256::SecretKey, String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(&r.secret)
        .map_err(|e| e.to_string())?;
    p256::SecretKey::from_slice(&bytes).map_err(|e| e.to_string())
}

/// The notification in a decrypted push message (`PushPayload`).
pub fn notification(json: &str) -> Option<protocol::Notification> {
    serde_json::from_str::<protocol::PushPayload>(json)
        .ok()
        .map(|p| p.event.notification)
}

/// The engine of the pod the app has open, when it is `instance`'s.
fn live(instance: &str) -> Option<PodEngine> {
    lock(&LIVE)
        .clone()
        .filter(|e| instance_for(e.base()) == instance)
}

/// Readies push for the pod the app opened (on a background thread: it
/// asks the pod): its keys, its saved endpoint sent to the pod again, and
/// the instance to ask the distributor for an endpoint for, with the pod's
/// VAPID key (base64url; the embedded FCM distributor registers with it).
/// `None` without a session, or when the pod sends no push.
pub fn set_up(store: &PushStore, engine: &PodEngine, log: fn(&str)) -> Option<(String, String)> {
    *lock(&LIVE) = Some(engine.clone());
    if !engine.signed_in() {
        return None;
    }
    let vapid = engine
        .push_public_key()
        .map_err(|e| log(&format!("push: {}: {e}", engine.base())))
        .ok()?;
    let instance = instance_for(engine.base());
    let keys = store
        .keys(&instance)
        .map_err(|e| log(&format!("push keys: {e}")))
        .ok()?;
    if let Some(endpoint) = store.endpoint(&instance)
        && let Err(e) = engine.register_push(&endpoint, &keys.p256dh, &keys.auth)
    {
        log(&format!("push registration: {e}"));
    }
    Some((instance, vapid))
}

/// The distributor gave `instance` an endpoint: kept, and sent to its pod
/// when the app has that pod open (else the next time it opens it).
pub fn new_endpoint(store: &PushStore, instance: &str, endpoint: &str, log: fn(&str)) {
    if let Err(e) = store.set_endpoint(instance, Some(endpoint)) {
        log(&format!("push: saving the endpoint: {e}"));
    }
    if let Some(engine) = live(instance)
        && let Ok(keys) = store.keys(instance)
    {
        let endpoint = endpoint.to_owned();
        std::thread::spawn(move || {
            if let Err(e) = engine.register_push(&endpoint, &keys.p256dh, &keys.auth) {
                log(&format!("push registration: {e}"));
            }
        });
    }
}

/// The distributor ended `instance`'s registration: forgotten here and on
/// the pod, when it is open.
pub fn unregistered(store: &PushStore, instance: &str, log: fn(&str)) {
    if let Err(e) = store.set_endpoint(instance, None) {
        log(&format!("push: {e}"));
    }
    if let Some(engine) = live(instance) {
        std::thread::spawn(move || {
            if let Err(e) = engine.unregister_push() {
                log(&format!("push: unregistering: {e}"));
            }
        });
    }
}

/// A message for `instance` (decrypted by the connector through
/// `PushKeys`, that is [`PushStore::decrypt`]): given to its pod's engine
/// when the app has it open (its inbox and count). The notification to
/// post, unless the app shows that pod in the foreground (its own bell
/// shows it then).
pub fn arrived(
    instance: &str,
    json: &str,
    foreground: impl FnOnce() -> bool,
) -> Result<Option<protocol::Notification>, String> {
    let n = notification(json).ok_or("not a notification")?;
    match live(instance) {
        Some(engine) => {
            engine.handle_push_message(json);
            Ok((!foreground()).then_some(n))
        }
        None => Ok(Some(n)),
    }
}

/// The Android side: `NativePush`'s native methods (the receiver), the
/// distributor registration and the notifications, through JNI.
#[cfg(all(target_os = "android", feature = "window"))]
pub mod android {
    use jni::errors::ThrowRuntimeExAndDefault;
    use jni::objects::{JByteArray, JClass, JObject, JObjectArray, JString, JValue};
    use jni::sys::jboolean;
    use jni::{EnvUnowned, jni_sig, jni_str};

    use super::{PushStore, TAP_LINK};

    fn log(m: &str) {
        crate::android::log(m);
    }

    /// The store in the app's private files (`Context.getFilesDir`).
    pub fn store(files: &std::path::Path) -> PushStore {
        PushStore::new(files.join("push.json"))
    }

    fn files_dir(env: &mut jni::Env, context: &JObject) -> jni::errors::Result<std::path::PathBuf> {
        let dir = env
            .call_method(
                context,
                jni_str!("getFilesDir"),
                jni_sig!("()Ljava/io/File;"),
                &[],
            )?
            .l()?;
        let path = env
            .call_method(
                &dir,
                jni_str!("getAbsolutePath"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        Ok(env.cast_local::<JString>(path)?.try_to_string(env)?.into())
    }

    fn text(env: &mut jni::Env, s: &JString) -> jni::errors::Result<String> {
        if s.is_null() {
            Ok(String::new())
        } else {
            s.try_to_string(env)
        }
    }

    /// `NativePush.endpoint`: a new endpoint for `instance`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_theconstruct_android_NativePush_endpoint<'l>(
        mut env: EnvUnowned<'l>,
        _class: JClass<'l>,
        context: JObject<'l>,
        endpoint: JString<'l>,
        instance: JString<'l>,
    ) {
        env.with_env(|env| -> jni::errors::Result<()> {
            let (endpoint, instance) = (text(env, &endpoint)?, text(env, &instance)?);
            // The host only: FCM (fcm.googleapis.com) or the distributor's server.
            let host = endpoint.split('/').nth(2).unwrap_or("?");
            log(&format!("push: new endpoint for {instance} at {host}"));
            let store = store(&files_dir(env, &context)?);
            super::new_endpoint(&store, &instance, &endpoint, log);
            Ok(())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `NativePush.message`: a push message for `instance`, decrypted by
    /// the connector with `PushKeys` (that is, by [`PushStore::decrypt`]).
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_theconstruct_android_NativePush_message<'l>(
        mut env: EnvUnowned<'l>,
        _class: JClass<'l>,
        context: JObject<'l>,
        message: JByteArray<'l>,
        decrypted: jboolean,
        instance: JString<'l>,
    ) {
        env.with_env(|env| -> jni::errors::Result<()> {
            let instance = text(env, &instance)?;
            if !decrypted {
                log(&format!(
                    "push message for {instance} dropped: not decrypted"
                ));
                return Ok(());
            }
            let body = env.convert_byte_array(&message)?;
            let Ok(json) = String::from_utf8(body) else {
                log(&format!("push message for {instance} dropped: not text"));
                return Ok(());
            };
            let store = store(&files_dir(env, &context)?);
            let shown = super::arrived(&instance, &json, || {
                foreground(env).unwrap_or_else(|_| {
                    env.exception_clear();
                    false
                })
            });
            match shown {
                Ok(Some(n)) => {
                    let value = serde_json::to_value(&n).unwrap_or_default();
                    match store.remember_tap(&instance, value) {
                        Ok(token) => {
                            if let Err(e) = post(env, &context, &n, &token) {
                                log(&format!("push: notification: {e}"));
                                env.exception_clear();
                            }
                        }
                        Err(e) => log(&format!("push: {e}")),
                    }
                }
                Ok(None) => log("push: shown in the open app"),
                Err(e) => log(&format!("push message for {instance} dropped: {e}")),
            }
            Ok(())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `NativePush.unregistered`: the distributor ended `instance`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_theconstruct_android_NativePush_unregistered<'l>(
        mut env: EnvUnowned<'l>,
        _class: JClass<'l>,
        context: JObject<'l>,
        instance: JString<'l>,
    ) {
        env.with_env(|env| -> jni::errors::Result<()> {
            let instance = text(env, &instance)?;
            log(&format!("push: {instance} unregistered"));
            super::unregistered(&store(&files_dir(env, &context)?), &instance, log);
            Ok(())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `NativePush.failed`: the distributor refused `instance` (`reason`:
    /// the connector's `FailedReason`, e.g. `INTERNAL_ERROR` from the
    /// embedded FCM distributor on a phone without Play services).
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_theconstruct_android_NativePush_failed<'l>(
        mut env: EnvUnowned<'l>,
        _class: JClass<'l>,
        _context: JObject<'l>,
        reason: JString<'l>,
        instance: JString<'l>,
    ) {
        env.with_env(|env| -> jni::errors::Result<()> {
            log(&format!(
                "push: registration failed for {}: {}",
                text(env, &instance)?,
                text(env, &reason)?
            ));
            Ok(())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `NativePush.decrypt` (`PushKeys.decrypt`): a message for `instance`
    /// decrypted with its keys (RFC 8291), or an exception, on which the
    /// connector hands the message on undecrypted (and `message` drops it).
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_theconstruct_android_NativePush_decrypt<'l>(
        mut env: EnvUnowned<'l>,
        _class: JClass<'l>,
        context: JObject<'l>,
        instance: JString<'l>,
        sealed: JByteArray<'l>,
    ) -> JByteArray<'l> {
        env.with_env(|env| -> jni::errors::Result<JByteArray<'l>> {
            let instance = text(env, &instance)?;
            let body = env.convert_byte_array(&sealed)?;
            match store(&files_dir(env, &context)?).decrypt(&instance, &body) {
                Ok(json) => env.byte_array_from_slice(json.as_bytes()),
                Err(e) => {
                    log(&format!("push: decrypting for {instance}: {e}"));
                    env.throw_new(
                        jni::jni_str!("java/lang/IllegalStateException"),
                        jni::jni_str!("push message not decrypted"),
                    )?;
                    Ok(JByteArray::default())
                }
            }
        })
        .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `NativePush.generate` (`PushKeys.generate`): keys for `instance`,
    /// made unless it has some.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_theconstruct_android_NativePush_generate<'l>(
        mut env: EnvUnowned<'l>,
        _class: JClass<'l>,
        context: JObject<'l>,
        instance: JString<'l>,
    ) {
        env.with_env(|env| -> jni::errors::Result<()> {
            let instance = text(env, &instance)?;
            if let Err(e) = store(&files_dir(env, &context)?).keys(&instance) {
                log(&format!("push keys: {e}"));
            }
            Ok(())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `NativePush.publicKeys` (`PushKeys.getPublicKeySet`): `{p256dh,
    /// auth}` of `instance`, or null without keys.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_theconstruct_android_NativePush_publicKeys<'l>(
        mut env: EnvUnowned<'l>,
        _class: JClass<'l>,
        context: JObject<'l>,
        instance: JString<'l>,
    ) -> JObjectArray<'l> {
        env.with_env(|env| -> jni::errors::Result<JObjectArray<'l>> {
            let instance = text(env, &instance)?;
            let store = store(&files_dir(env, &context)?);
            if !store.exists(&instance) {
                return Ok(JObjectArray::default());
            }
            let Ok(keys) = store.keys(&instance) else {
                return Ok(JObjectArray::default());
            };
            let p256dh = env.new_string(&keys.p256dh)?;
            let auth = env.new_string(&keys.auth)?;
            let array = env.new_object_array(2, jni::jni_str!("java/lang/String"), &p256dh)?;
            array.set_element(env, 1, &auth)?;
            Ok(array)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `NativePush.exists` (`PushKeys.exists`).
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_theconstruct_android_NativePush_exists<'l>(
        mut env: EnvUnowned<'l>,
        _class: JClass<'l>,
        context: JObject<'l>,
        instance: JString<'l>,
    ) -> jboolean {
        env.with_env(|env| -> jni::errors::Result<jboolean> {
            let instance = text(env, &instance)?;
            Ok(store(&files_dir(env, &context)?).exists(&instance))
        })
        .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `NativePush.delete` (`PushKeys.delete`): the registration ended.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_theconstruct_android_NativePush_delete<'l>(
        mut env: EnvUnowned<'l>,
        _class: JClass<'l>,
        context: JObject<'l>,
        instance: JString<'l>,
    ) {
        env.with_env(|env| -> jni::errors::Result<()> {
            let instance = text(env, &instance)?;
            if let Err(e) = store(&files_dir(env, &context)?).forget(&instance) {
                log(&format!("push: forgetting {instance}: {e}"));
            }
            Ok(())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// Whether the app is in the foreground (`ActivityManager.getMyMemoryState`).
    fn foreground(env: &mut jni::Env) -> jni::errors::Result<bool> {
        let info = env.new_object(
            jni_str!("android/app/ActivityManager$RunningAppProcessInfo"),
            jni_sig!("()V"),
            &[],
        )?;
        env.call_static_method(
            jni_str!("android/app/ActivityManager"),
            jni_str!("getMyMemoryState"),
            jni_sig!("(Landroid/app/ActivityManager$RunningAppProcessInfo;)V"),
            &[JValue::Object(&info)],
        )?;
        // IMPORTANCE_FOREGROUND.
        Ok(env
            .get_field(&info, jni_str!("importance"), jni_sig!("I"))?
            .i()?
            <= 100)
    }

    /// A class of the app's (or a library's in its dex), from any thread:
    /// a thread Rust attached finds only the system's classes by name.
    fn app_class<'l>(
        env: &mut jni::Env<'l>,
        context: &JObject,
        name: &str,
    ) -> jni::errors::Result<JClass<'l>> {
        let loader = env
            .call_method(
                context,
                jni_str!("getClassLoader"),
                jni_sig!("()Ljava/lang/ClassLoader;"),
                &[],
            )?
            .l()?;
        let name = env.new_string(name)?;
        let class = env
            .call_method(
                &loader,
                jni_str!("loadClass"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
                &[JValue::Object(&name)],
            )?
            .l()?;
        env.cast_local::<JClass>(class)
    }

    /// Asks the distributor for an endpoint for `instance` (the connector's
    /// `UnifiedPush.register`, with the pod's `vapid` key and `PushKeys`, so
    /// the keys stay here); the endpoint comes to the receiver. The
    /// distributor app saved before while it is installed, else the first
    /// distributor app, else the embedded FCM distributor (this app itself;
    /// it fails later without Play services), so an app installed later
    /// takes over from the fallback. False when there is none.
    pub fn register(
        env: &mut jni::Env,
        context: &JObject,
        instance: &str,
        vapid: &str,
    ) -> jni::errors::Result<bool> {
        let up = app_class(
            env,
            context,
            "org.unifiedpush.android.connector.UnifiedPush",
        )?;
        let saved = env
            .call_static_method(
                &up,
                jni_str!("getSavedDistributor"),
                jni_sig!("(Landroid/content/Context;)Ljava/lang/String;"),
                &[JValue::Object(context)],
            )?
            .l()?;
        let saved = text(env, &env.cast_local::<JString>(saved)?)?;
        let own = env
            .call_method(
                context,
                jni_str!("getPackageName"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        let own = text(env, &env.cast_local::<JString>(own)?)?;
        let list = env
            .call_static_method(
                &up,
                jni_str!("getDistributors"),
                jni_sig!("(Landroid/content/Context;)Ljava/util/List;"),
                &[JValue::Object(context)],
            )?
            .l()?;
        let n = env
            .call_method(&list, jni_str!("size"), jni_sig!("()I"), &[])?
            .i()?;
        let mut names = Vec::new();
        for i in 0..n {
            let d = env
                .call_method(
                    &list,
                    jni_str!("get"),
                    jni_sig!("(I)Ljava/lang/Object;"),
                    &[JValue::Int(i)],
                )?
                .l()?;
            names.push(text(env, &env.cast_local::<JString>(d)?)?);
        }
        // A distributor app over the embedded one (the user installed it to
        // be used, also after the fallback was chosen): the saved app while
        // it is still installed, else the first app, else the embedded one.
        let Some(pick) = names
            .iter()
            .find(|d| **d == saved && **d != own)
            .or_else(|| names.iter().find(|d| **d != own))
            .or_else(|| names.iter().find(|d| **d == own))
        else {
            return Ok(false);
        };
        log(&format!("push: distributor {pick}"));
        if *pick != saved {
            let pick = env.new_string(pick)?;
            env.call_static_method(
                &up,
                jni_str!("saveDistributor"),
                jni_sig!("(Landroid/content/Context;Ljava/lang/String;)V"),
                &[JValue::Object(context), JValue::Object(&pick)],
            )?;
        }
        let keys_class = app_class(env, context, "org.theconstruct.android.PushKeys")?;
        let app_context = env
            .call_method(
                context,
                jni_str!("getApplicationContext"),
                jni_sig!("()Landroid/content/Context;"),
                &[],
            )?
            .l()?;
        let keys = env.new_object(
            &keys_class,
            jni_sig!("(Landroid/content/Context;)V"),
            &[JValue::Object(&app_context)],
        )?;
        let instance = env.new_string(instance)?;
        let vapid = env.new_string(vapid)?;
        env.call_static_method(
            &up,
            jni_str!("register"),
            jni_sig!("(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Lorg/unifiedpush/android/connector/keys/KeyManager;)V"),
            &[
                JValue::Object(context),
                JValue::Object(&instance),
                JValue::Object(&JObject::null()),
                JValue::Object(&vapid),
                JValue::Object(&keys),
            ],
        )?;
        Ok(true)
    }

    fn sdk(env: &mut jni::Env) -> jni::errors::Result<i32> {
        env.get_static_field(
            jni_str!("android/os/Build$VERSION"),
            jni_str!("SDK_INT"),
            jni_sig!("I"),
        )?
        .i()
    }

    /// `request_notification_permission`: Android's prompt while it still
    /// shows one (never asked, or denied once), else the app's
    /// notification settings.
    pub fn ask_permission(
        env: &mut jni::Env,
        activity: &JObject,
        store: &PushStore,
    ) -> jni::errors::Result<()> {
        let perm = env.new_string("android.permission.POST_NOTIFICATIONS")?;
        if sdk(env)? >= 33 {
            let granted = env
                .call_method(
                    activity,
                    jni_str!("checkSelfPermission"),
                    jni_sig!("(Ljava/lang/String;)I"),
                    &[JValue::Object(&perm)],
                )?
                .i()?
                == 0;
            let rationale = env
                .call_method(
                    activity,
                    jni_str!("shouldShowRequestPermissionRationale"),
                    jni_sig!("(Ljava/lang/String;)Z"),
                    &[JValue::Object(&perm)],
                )?
                .z()?;
            if !granted && (rationale || !store.asked_before()) {
                return crate::bridge::ask_notifications(env, activity);
            }
        }
        // Settings.ACTION_APP_NOTIFICATION_SETTINGS, Settings.EXTRA_APP_PACKAGE.
        let action = env.new_string("android.settings.APP_NOTIFICATION_SETTINGS")?;
        let intent = env.new_object(
            jni_str!("android/content/Intent"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(&action)],
        )?;
        let key = env.new_string("android.provider.extra.APP_PACKAGE")?;
        let pkg = env
            .call_method(
                activity,
                jni_str!("getPackageName"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        env.call_method(
            &intent,
            jni_str!("putExtra"),
            jni_sig!("(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;"),
            &[JValue::Object(&key), JValue::Object(&pkg)],
        )?;
        env.call_method(
            activity,
            jni_str!("startActivity"),
            jni_sig!("(Landroid/content/Intent;)V"),
            &[JValue::Object(&intent)],
        )?;
        Ok(())
    }

    /// Posts `n` (tag: its id, so a second arrival replaces it) in the
    /// channel `<app_id>.<channel>`, as the Compose app does. A tap opens
    /// `ConstructActivity` (explicitly) with the tap link of `token`.
    fn post(
        env: &mut jni::Env,
        context: &JObject,
        n: &protocol::Notification,
        token: &str,
    ) -> jni::errors::Result<()> {
        let service = env.new_string("notification")?;
        let nm = env
            .call_method(
                context,
                jni_str!("getSystemService"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                &[JValue::Object(&service)],
            )?
            .l()?;
        // The channel, made the first time (the user's choices for it live
        // in Android's settings).
        let id = env.new_string(format!("{}.{}", n.app_id, n.channel))?;
        let existing = env
            .call_method(
                &nm,
                jni_str!("getNotificationChannel"),
                jni_sig!("(Ljava/lang/String;)Landroid/app/NotificationChannel;"),
                &[JValue::Object(&id)],
            )?
            .l()?;
        if existing.is_null() {
            let mut title: String = n.channel.clone();
            if let Some(c) = title.get(..1) {
                title = c.to_uppercase() + &title[1..];
            }
            let name = env.new_string(title)?;
            // IMPORTANCE_DEFAULT.
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
        }
        let builder = env.new_object(
            jni_str!("android/app/Notification$Builder"),
            jni_sig!("(Landroid/content/Context;Ljava/lang/String;)V"),
            &[JValue::Object(context), JValue::Object(&id)],
        )?;
        let set = |env: &mut jni::Env, name, sig, arg: JValue| {
            env.call_method(&builder, name, sig, &[arg]).map(|_| ())
        };
        // The app's notification icon (ic_stat_notification), else Android's.
        let res = env
            .call_method(
                context,
                jni_str!("getResources"),
                jni_sig!("()Landroid/content/res/Resources;"),
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
        let (icon_name, kind) = (
            env.new_string("ic_stat_notification")?,
            env.new_string("drawable")?,
        );
        let mut icon = env
            .call_method(
                &res,
                jni_str!("getIdentifier"),
                jni_sig!("(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)I"),
                &[
                    JValue::Object(&icon_name),
                    JValue::Object(&kind),
                    JValue::Object(&pkg),
                ],
            )?
            .i()?;
        if icon == 0 {
            icon = env
                .get_static_field(
                    jni_str!("android/R$drawable"),
                    jni_str!("ic_dialog_info"),
                    jni_sig!("I"),
                )?
                .i()?;
        }
        set(
            env,
            jni_str!("setSmallIcon"),
            jni_sig!("(I)Landroid/app/Notification$Builder;"),
            JValue::Int(icon),
        )?;
        let title = env.new_string(&n.title)?;
        set(
            env,
            jni_str!("setContentTitle"),
            jni_sig!("(Ljava/lang/CharSequence;)Landroid/app/Notification$Builder;"),
            JValue::Object(&title),
        )?;
        if !n.body.trim().is_empty() {
            let body = env.new_string(&n.body)?;
            set(
                env,
                jni_str!("setContentText"),
                jni_sig!("(Ljava/lang/CharSequence;)Landroid/app/Notification$Builder;"),
                JValue::Object(&body),
            )?;
            let style = env.new_object(
                jni_str!("android/app/Notification$BigTextStyle"),
                jni_sig!("()V"),
                &[],
            )?;
            env.call_method(
                &style,
                jni_str!("bigText"),
                jni_sig!("(Ljava/lang/CharSequence;)Landroid/app/Notification$BigTextStyle;"),
                &[JValue::Object(&body)],
            )?;
            set(
                env,
                jni_str!("setStyle"),
                jni_sig!("(Landroid/app/Notification$Style;)Landroid/app/Notification$Builder;"),
                JValue::Object(&style),
            )?;
        }
        if let Some(g) = &n.group_key {
            let group = env.new_string(format!("{}:{g}", n.app_id))?;
            set(
                env,
                jni_str!("setGroup"),
                jni_sig!("(Ljava/lang/String;)Landroid/app/Notification$Builder;"),
                JValue::Object(&group),
            )?;
        }
        set(
            env,
            jni_str!("setWhen"),
            jni_sig!("(J)Landroid/app/Notification$Builder;"),
            JValue::Long(n.created_at.saturating_mul(1000)),
        )?;
        set(
            env,
            jni_str!("setShowWhen"),
            jni_sig!("(Z)Landroid/app/Notification$Builder;"),
            JValue::Bool(true),
        )?;
        set(
            env,
            jni_str!("setAutoCancel"),
            jni_sig!("(Z)Landroid/app/Notification$Builder;"),
            JValue::Bool(true),
        )?;
        // The tap: ACTION_VIEW of the tap link, to this app's activity only.
        let link = env.new_string(format!("{TAP_LINK}{token}"))?;
        let uri = env
            .call_static_method(
                jni_str!("android/net/Uri"),
                jni_str!("parse"),
                jni_sig!("(Ljava/lang/String;)Landroid/net/Uri;"),
                &[JValue::Object(&link)],
            )?
            .l()?;
        let view = env.new_string("android.intent.action.VIEW")?;
        let intent = env.new_object(
            jni_str!("android/content/Intent"),
            jni_sig!("(Ljava/lang/String;Landroid/net/Uri;)V"),
            &[JValue::Object(&view), JValue::Object(&uri)],
        )?;
        let activity = env.new_string("org.theconstruct.android.ConstructActivity")?;
        env.call_method(
            &intent,
            jni_str!("setClassName"),
            jni_sig!("(Landroid/content/Context;Ljava/lang/String;)Landroid/content/Intent;"),
            &[JValue::Object(context), JValue::Object(&activity)],
        )?;
        // FLAG_ACTIVITY_NEW_TASK.
        env.call_method(
            &intent,
            jni_str!("addFlags"),
            jni_sig!("(I)Landroid/content/Intent;"),
            &[JValue::Int(0x1000_0000)],
        )?;
        let code = token
            .bytes()
            .fold(0i32, |h, b| h.wrapping_mul(31).wrapping_add(b as i32));
        // FLAG_IMMUTABLE | FLAG_UPDATE_CURRENT.
        let pending = env
            .call_static_method(
                jni_str!("android/app/PendingIntent"),
                jni_str!("getActivity"),
                jni_sig!("(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;"),
                &[
                    JValue::Object(context),
                    JValue::Int(code),
                    JValue::Object(&intent),
                    JValue::Int(0x0400_0000 | 0x0800_0000),
                ],
            )?
            .l()?;
        set(
            env,
            jni_str!("setContentIntent"),
            jni_sig!("(Landroid/app/PendingIntent;)Landroid/app/Notification$Builder;"),
            JValue::Object(&pending),
        )?;
        let built = env
            .call_method(
                &builder,
                jni_str!("build"),
                jni_sig!("()Landroid/app/Notification;"),
                &[],
            )?
            .l()?;
        let tag = env.new_string(&n.id)?;
        env.call_method(
            &nm,
            jni_str!("notify"),
            jni_sig!("(Ljava/lang/String;ILandroid/app/Notification;)V"),
            &[JValue::Object(&tag), JValue::Int(1), JValue::Object(&built)],
        )?;
        log(&format!("push: posted {}", n.id));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (PushStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "construct-push-{}-{}",
            std::process::id(),
            URL_SAFE_NO_PAD.encode(random::<6>().unwrap())
        ));
        (PushStore::new(dir.join("push.json")), dir)
    }

    /// What the pod sends (`PushPayload`), encrypted to `keys` as
    /// `web-push-native` does on the pod.
    fn sealed(keys: &Keys, json: &str) -> Vec<u8> {
        let public =
            p256::PublicKey::from_sec1_bytes(&URL_SAFE_NO_PAD.decode(&keys.p256dh).unwrap())
                .unwrap();
        let auth = URL_SAFE_NO_PAD.decode(&keys.auth).unwrap();
        web_push_native::encrypt(
            json.as_bytes().to_vec(),
            &public,
            &web_push_native::Auth::clone_from_slice(&auth),
        )
        .unwrap()
    }

    const PAYLOAD: &str = r#"{"v":1,"notification":{"id":"n1","app_id":"events","channel":"matches",
        "title":"Night Owls at The Harbour Room","body":"Fri 20:00","target_uri":"construct://events/e1",
        "layout":"gigs","created_at":1700000000,"read":false},"unread_total":3}"#;

    #[test]
    fn keys_are_kept_per_server_and_decrypt_what_the_pod_sends() {
        let (s, dir) = store();
        let a = instance_for("https://pod.example/");
        assert_eq!(a, "server:https://pod.example");
        let keys = s.keys(&a).unwrap();
        assert_eq!(s.keys(&a).unwrap(), keys, "made once");
        assert_eq!(URL_SAFE_NO_PAD.decode(&keys.p256dh).unwrap().len(), 65);
        assert_eq!(URL_SAFE_NO_PAD.decode(&keys.auth).unwrap().len(), 16);
        let other = s.keys("server:https://other.example").unwrap();
        assert_ne!(other, keys, "a server never shares another's keys");

        let json = s.decrypt(&a, &sealed(&keys, PAYLOAD)).unwrap();
        let n = notification(&json).unwrap();
        assert_eq!((n.id.as_str(), n.layout.as_deref()), ("n1", Some("gigs")));
        assert!(
            s.decrypt("server:https://other.example", &sealed(&keys, PAYLOAD))
                .is_err(),
            "another server's keys don't open it"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn endpoints_are_kept_and_forgotten() {
        let (s, dir) = store();
        let a = instance_for("https://pod.example");
        s.keys(&a).unwrap();
        assert_eq!(s.endpoint(&a), None);
        s.set_endpoint(&a, Some("https://push.example/up/abc"))
            .unwrap();
        assert_eq!(
            s.endpoint(&a).as_deref(),
            Some("https://push.example/up/abc")
        );
        s.set_endpoint(&a, None).unwrap();
        assert_eq!(s.endpoint(&a), None);
        assert!(s.exists(&a));
        s.set_endpoint(&a, Some("https://push.example/up/abc"))
            .unwrap();
        s.forget(&a).unwrap();
        assert!(!s.exists(&a), "keys and endpoint go together");
        assert_eq!(s.endpoint(&a), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_tap_opens_only_a_notification_it_was_given_for_and_once() {
        let (s, dir) = store();
        let n = serde_json::json!({ "id": "n1", "layout": "gigs" });
        let token = s
            .remember_tap("server:https://pod.example", n.clone())
            .unwrap();
        let link = format!("{TAP_LINK}{token}");
        assert_eq!(tap_token(&link), Some(token.as_str()));
        assert_eq!(tap_token("construct://notification/"), None);
        assert_eq!(tap_token("construct://oauth?code=x"), None);
        assert_eq!(s.take_tap("made-up"), None);
        assert_eq!(
            s.take_tap(&token),
            Some(Tapped {
                instance: "server:https://pod.example".into(),
                notification: n
            })
        );
        assert_eq!(s.take_tap(&token), None, "once");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_the_newest_taps_are_kept() {
        let (s, dir) = store();
        let first = s.remember_tap("i", Value::Null).unwrap();
        for _ in 0..TAPS_KEPT {
            s.remember_tap("i", Value::Null).unwrap();
        }
        assert_eq!(s.take_tap(&first), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_message_without_an_open_pod_is_posted() {
        let (s, dir) = store();
        let a = instance_for("https://closed.example");
        let keys = s.keys(&a).unwrap();
        let json = s.decrypt(&a, &sealed(&keys, PAYLOAD)).unwrap();
        let n = arrived(&a, &json, || true).unwrap();
        assert_eq!(
            n.map(|n| n.title),
            Some("Night Owls at The Harbour Room".into())
        );
        assert!(arrived(&a, "not a notification", || false).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
