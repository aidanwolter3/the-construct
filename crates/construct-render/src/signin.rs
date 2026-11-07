//! Signing in to an upstream (Pixelfed) from the Rust clients, through the
//! engine's own flow (`client_core::auth`, decision D19): the engine
//! registers the client with the pod, the platform opens the browser, the
//! redirect comes back and the engine exchanges the code.
//!
//! How the redirect comes back is the platform's: the desktop viewer gets
//! it on a loopback listener the engine runs (RFC 8252 §7.3); the Android
//! app gets it as a link that opens the app (the bridge's
//! `PlatformEvent::Link`, handed to [`crate::engine::PodEngine`]): the
//! verified App Link `https://<pod host>/oauth/android` for the pod on
//! [`APP_LINK_HOST`] once Android has verified it, else `construct://oauth`
//! (decision `android-app-link-sign-in-redirect`, as `AuthRedirects` in the
//! Compose app).

use std::sync::Arc;

use client_core::{RedirectMode, UrlOpener};

/// The custom-scheme redirect: local pods, other servers, unverified
/// builds, and the pod's fallback page's link.
pub const CUSTOM_SCHEME: &str = "construct://oauth";
/// Path of the App Link on the pod (`pod_host::app_links::REDIRECT_PATH`).
pub const APP_LINK_PATH: &str = "/oauth/android";
/// The pod whose App Link the Android app's manifest claims
/// (`android/AndroidManifest.xml`): the staging pod, as the main app's
/// default `-PconstructAppLinkHost`.
pub const APP_LINK_HOST: &str = "the-construct-pod.fly.dev";

/// `https://<host>/oauth/android` when `base` is that host over https on
/// the default port (the server that serves the App Link's
/// `assetlinks.json`), else `None`.
pub fn app_link_for(base: &str, host: &str) -> Option<String> {
    let host = host.trim().to_ascii_lowercase();
    let base = base.trim().trim_end_matches('/');
    let rest = base
        .get(..8)
        .filter(|s| s.eq_ignore_ascii_case("https://"))
        .map(|_| &base[8..])?;
    (!host.is_empty() && rest.eq_ignore_ascii_case(&host))
        .then(|| format!("https://{host}{APP_LINK_PATH}"))
}

/// The redirect a phone registers for `base`: its App Link when
/// [`app_link_for`] has one and the device hands that link to this app
/// (`verified`), else [`CUSTOM_SCHEME`].
pub fn phone_redirect(base: &str, host: &str, verified: impl FnOnce() -> bool) -> RedirectMode {
    let uri = app_link_for(base, host)
        .filter(|_| verified())
        .unwrap_or_else(|| CUSTOM_SCHEME.to_owned());
    RedirectMode::Custom(uri)
}

/// Whether a link that opened the app is a sign-in redirect:
/// `construct://oauth[/…][?…]` or `https://<host>/oauth/android[?…]`. The
/// engine still checks its `state`.
pub fn is_redirect(uri: &str, host: &str) -> bool {
    let base = uri.trim();
    let base = base.split(['#', '?']).next().unwrap_or(base);
    let lower = base.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix(CUSTOM_SCHEME) {
        return rest.is_empty() || rest.starts_with('/');
    }
    let host = host.trim().to_ascii_lowercase();
    !host.is_empty() && lower.strip_prefix(&format!("https://{host}")) == Some(APP_LINK_PATH)
}

/// How a platform signs in: what opens the browser, and the redirect
/// for a server.
#[derive(Clone)]
pub struct SignIn {
    pub opener: Arc<dyn UrlOpener>,
    pub redirect: Arc<dyn Fn(&str) -> RedirectMode + Send + Sync>,
}

impl SignIn {
    /// A platform's sign-in: `open` opens an address in the browser (false
    /// when it can't), `redirect` picks the redirect for a server.
    pub fn new(
        open: impl Fn(&str) -> bool + Send + Sync + 'static,
        redirect: impl Fn(&str) -> RedirectMode + Send + Sync + 'static,
    ) -> SignIn {
        SignIn {
            opener: Arc::new(Opener(open)),
            redirect: Arc::new(redirect),
        }
    }

    /// The desktop's: the system browser, and a loopback redirect.
    pub fn desktop() -> SignIn {
        SignIn::new(open_in_browser, |_| RedirectMode::Loopback)
    }
}

struct Opener<F>(F);

impl<F: Fn(&str) -> bool + Send + Sync> UrlOpener for Opener<F> {
    fn open(&self, url: &str) -> bool {
        (self.0)(url)
    }
}

/// Opens an address with the desktop's opener (`open`, `xdg-open`,
/// `start`); false when it can't start.
pub fn open_in_browser(url: &str) -> bool {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    match cmd.arg(url).spawn() {
        Ok(_) => true,
        Err(e) => {
            eprintln!("construct-render: can't open the browser: {e}");
            false
        }
    }
}

/// The Android app's: the browser through an `ACTION_VIEW` intent, and
/// the App Link or `construct://oauth`, which come back through the
/// bridge (`PlatformEvent::Link`).
#[cfg(all(target_os = "android", feature = "window"))]
pub(crate) fn android(app: &winit::platform::android::activity::AndroidApp) -> SignIn {
    use crate::android::{log, view_url, with_activity};
    let (browser, activity) = (app.clone(), app.clone());
    SignIn::new(
        move |url| {
            view_url(&browser, url)
                .map_err(|e| log(&format!("can't open the sign-in page: {e}")))
                .is_ok()
        },
        move |base| {
            let redirect = phone_redirect(base, APP_LINK_HOST, || {
                with_activity(&activity, |env, a| app_link_verified(env, a, APP_LINK_HOST))
                    .unwrap_or_else(|e| {
                        log(&format!("App Link state of {APP_LINK_HOST}: {e}"));
                        false
                    })
            });
            log(&format!("sign-in redirect for {base}: {redirect:?}"));
            redirect
        },
    )
}

/// Whether Android hands `https://<host>/…` links to this app
/// (`DomainVerificationManager`, Android 12+): verified against the pod's
/// `assetlinks.json`, or turned on by the user under "Open by default".
/// Android 8-11 verify at install and have no API to ask; an unverified
/// link there shows a chooser that includes the app, so it is used.
#[cfg(all(target_os = "android", feature = "window"))]
pub fn app_link_verified(
    env: &mut jni::Env,
    activity: &jni::objects::JObject,
    host: &str,
) -> jni::errors::Result<bool> {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};
    let sdk = env
        .get_static_field(
            jni_str!("android/os/Build$VERSION"),
            jni_str!("SDK_INT"),
            jni_sig!("I"),
        )?
        .i()?;
    if sdk < 31 {
        return Ok(true);
    }
    let name = env.new_string("domain_verification")?;
    let manager = env
        .call_method(
            activity,
            jni_str!("getSystemService"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
            &[JValue::Object(&name)],
        )?
        .l()?;
    if manager.is_null() {
        return Ok(false);
    }
    let pkg = env
        .call_method(
            activity,
            jni_str!("getPackageName"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    let state = env
        .call_method(
            &manager,
            jni_str!("getDomainVerificationUserState"),
            jni_sig!(
                "(Ljava/lang/String;)Landroid/content/pm/verify/domain/DomainVerificationUserState;"
            ),
            &[JValue::Object(&pkg)],
        )?
        .l()?;
    if state.is_null() {
        return Ok(false);
    }
    let map = env
        .call_method(
            &state,
            jni_str!("getHostToStateMap"),
            jni_sig!("()Ljava/util/Map;"),
            &[],
        )?
        .l()?;
    let key = env.new_string(host)?;
    let value = env
        .call_method(
            &map,
            jni_str!("get"),
            jni_sig!("(Ljava/lang/Object;)Ljava/lang/Object;"),
            &[JValue::Object(&key)],
        )?
        .l()?;
    if value.is_null() {
        return Ok(false);
    }
    let n = env
        .call_method(&value, jni_str!("intValue"), jni_sig!("()I"), &[])?
        .i()?;
    // DOMAIN_STATE_SELECTED (1), DOMAIN_STATE_VERIFIED (2).
    Ok(n == 1 || n == 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "pod.example";

    #[test]
    fn the_app_link_is_only_for_its_own_pod_over_https() {
        assert_eq!(
            app_link_for("https://pod.example/", HOST).as_deref(),
            Some("https://pod.example/oauth/android")
        );
        assert_eq!(
            app_link_for("HTTPS://Pod.Example", HOST).as_deref(),
            Some("https://pod.example/oauth/android")
        );
        assert_eq!(app_link_for("http://pod.example", HOST), None);
        assert_eq!(app_link_for("https://pod.example:8443", HOST), None);
        assert_eq!(app_link_for("https://other.example", HOST), None);
        assert_eq!(app_link_for("https://pod.example", ""), None);
    }

    #[test]
    fn a_phone_uses_the_app_link_once_verified_else_the_scheme() {
        let custom = |u: &str| RedirectMode::Custom(u.to_owned());
        assert_eq!(
            phone_redirect("https://pod.example", HOST, || true),
            custom("https://pod.example/oauth/android")
        );
        assert_eq!(
            phone_redirect("https://pod.example", HOST, || false),
            custom(CUSTOM_SCHEME)
        );
        assert_eq!(
            phone_redirect("http://127.0.0.1:8080", HOST, || panic!("not asked")),
            custom(CUSTOM_SCHEME)
        );
    }

    #[test]
    fn tells_sign_in_redirects_from_other_links() {
        assert!(is_redirect("construct://oauth?code=a&state=b", HOST));
        assert!(is_redirect("CONSTRUCT://oauth/x?code=a", HOST));
        assert!(is_redirect("construct://oauth", HOST));
        assert!(is_redirect(
            "https://pod.example/oauth/android?code=a&state=b",
            HOST
        ));
        assert!(!is_redirect("construct://oauthx?code=a", HOST));
        assert!(!is_redirect("construct://bridge?x=1", HOST));
        assert!(!is_redirect("https://pod.example/oauth/androidx", HOST));
        assert!(!is_redirect("https://other.example/oauth/android", HOST));
        assert!(!is_redirect("https://pod.example/oauth/android", ""));
    }
}
