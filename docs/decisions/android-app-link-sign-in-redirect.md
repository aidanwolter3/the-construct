# The Android sign-in redirect is a verified App Link on the pod, with `construct://oauth` as fallback

Date: 2026-11-03. Related: D19, D21, D40, D45, D46, security audit #31 (Info, "Custom scheme"), issue #37.
Supersedes the Android redirect of D19 (the rest of D19 stands).

The Android app received its OAuth redirect on the custom scheme `construct://oauth`. Any installed app can declare
the same scheme, and Android may then show a chooser or route the redirect to that app. PKCE (S256) and the `state`
check already stop a stolen `code` from being redeemed, but another app could still catch the redirect and break or
spoof the sign-in. An https link that Android has verified for our app (an App Link) can only open our app.

What App Links need, and the choices made:

- **A domain we control serving `/.well-known/assetlinks.json`** with the app's package and signing-cert SHA-256.
  The pod is the natural host: the app already trusts it, and staging (`the-construct-pod.fly.dev`) is the build's
  default server. The file is served by `pod-host`, not by `pod-core` or an adapter: it is about the Android client,
  which every app shares, so no app knowledge enters the generic crates (D40). It is configured per deployment:
  `CONSTRUCT_ANDROID_CERT_SHA256` (a list) and `CONSTRUCT_ANDROID_PACKAGE` (default `org.theconstruct.android`).
  With no fingerprint the route is 404, so nothing verifies and nothing changes.
- **The redirect URI is `https://<pod host>/oauth/android`.** The pod's Pixelfed allowlist accepts it only as an
  exact `PIXELFED_EXTRA_REDIRECTS` entry (staging sets it). A built-in rule such as "`https://<request Host>/oauth/
  android`" was rejected: behind a proxy that passes any `Host`, it would let someone register the pod's upstream app
  with a redirect on their own domain and collect other people's codes, which is what the allowlist exists to stop.
  The staging fake Pixelfed only registers `construct://` and loopback redirects (audit L6); it gets the same exact
  opt-in, `FAKE_PIXELFED_EXTRA_REDIRECTS`.
- **The app picks the redirect per server.** The manifest has an `autoVerify` https filter for one host,
  `-PconstructAppLinkHost` (default: the staging pod's host; also `BuildConfig.APP_LINK_HOST`). The engine for a
  server gets the App Link when the server is `https://<that host>` and the link is usable on this device; every
  other server (local pods over `adb reverse` or the emulator, self-hosted pods, older pods) keeps
  `construct://oauth`, which stays registered in the manifest and accepted by the pod. "Usable": Android 12+ is asked
  through `DomainVerificationManager` (verified, or turned on by the user under "Open by default"); Android 8-11
  verify at install and have no API, so the link is used, and if it did not verify Android shows a chooser that
  includes the app. The logic is `AuthRedirects` in shared code (unit-tested on the JVM); the engine needs no change,
  since it already matches a redirect by `state` whatever its URI.
- **A fallback page.** Browsers do not always hand a redirect to an app (Chrome keeps redirects no tap started,
  e.g. an authorize page that approves at once). The browser then loads `https://<pod>/oauth/android?code=…&state=…`
  from the pod itself, which answers with a page whose one button is an `intent://oauth?…#Intent;scheme=construct;
  package=<package>;end` link: Chrome and Firefox send it to that package only, as `construct://oauth?…`, and the
  engine completes the pending sign-in. The page runs no script, is `no-store`, sends no referrer, and only echoes a
  query of URL-safe characters (no `#`, `;`, quotes or angle brackets).
- **Desktop is unchanged** (loopback redirect, RFC 8252 §7.3).

**Debug vs release signing.** Verification compares the installed APK's signing certificate with the fingerprints in
`assetlinks.json`, so each signing key that should get App Links must be listed:

- *Release*: the developer's release (or Play App Signing) key. Its fingerprint is only known to the developer, so it
  is not in the repo; add it to `CONSTRUCT_ANDROID_CERT_SHA256` in `deploy/fly/pod.toml` (fingerprints are not
  secret: every APK carries its certificate).
- *Debug*: each machine's `~/.android/debug.keystore` has its own key. List the developer's debug fingerprint too to
  test App Links with `installDebug`. A debug build whose key is not listed is simply not verified: on Android 12+ it
  keeps `construct://oauth` (as before this decision); on 8-11 it gets the chooser.

Decision: the Android app signs in through the verified App Link `https://<pod host>/oauth/android` on the pod that
serves its `assetlinks.json`, with the pod's `/oauth/android` page as the browser fallback, and keeps
`construct://oauth` for every other server, unverified builds and older app builds.

Consequences and limits:

- `construct://oauth` is not dropped: older builds and local pods need it, and the fallback page uses it (aimed at
  our package). On those paths the old risk (another app shows up in a chooser) remains, still without code theft
  thanks to PKCE. Dropping it would need App Links for local pods, which can't verify (no public https host).
- An Android 8-11 device whose verification failed (e.g. no fingerprint configured yet) sees a chooser instead of
  going straight to the app. Configure the fingerprints before shipping a build with this filter to such devices.
- A build only verifies for one host. A self-hosted pod gets App Links only from a build made with
  `-PconstructAppLinkHost=<its host>` and its own fingerprints; otherwise it uses `construct://oauth`.
- Only the redirect changes: no protocol or schema change. The pod and fake changes ship with the server; the
  App Link needs a new Android build.
