# The Rust Android app falls back to the embedded FCM distributor, with connector 3.x and keys still in Rust

Date: 2026-11-07. Related: [android-native-push-unifiedpush](android-native-push-unifiedpush.md) (its connector
2.5.0 part is superseded), [android-native-bridge](android-native-bridge.md), D52 / [`../notifications.md`](../notifications.md),
issue #188.

The Rust app got closed-app push only through a distributor app (ntfy, ...): most phones have none. The Compose app
covers them with UnifiedPush's embedded FCM distributor.

Considered (APK sizes measured with `scripts/android_native.sh`'s own steps, javac + d8 without shrinking; today's
APK 10,352,744 B, 12,285 dex method references):

- **Connector 3.3.5 + embedded FCM distributor 3.1.0, keys in Rust** (chosen): +471 KB, 18,633 method refs. The
  distributor registers with Play services directly (the c2dm IPC, no Firebase SDK) using the pod's VAPID key as
  sender, so the endpoint is FCM's own Web Push endpoint (`fcm.googleapis.com/fcm/send/<token>`): no Firebase project,
  no `google-services.json`, no gateway, no server change. Needs connector 3.x: 3.1.0 refuses 2.5.0's registrations
  (no `vapid`, no `pi` extra).
- The same with the connector's own key store (as the Compose app has it): +1.95 MB, 62,910 method refs (Tink,
  protobuf, gson), close to one dex's 65,536; keys and decryption in Java libraries.
- **Embedded FCM distributor 2.5.0** with connector 2.5.0: +1.44 MB, 34,537 method refs (firebase-messaging 24,
  play-services, 22 androidx libraries); needs our own Firebase project (its values as resources), the libraries'
  resources merged (Play services' strings in every language), our receiver subclass to supply the endpoint, and a
  gateway or pod code sending through FCM's HTTP v1 API with a service-account secret.
- **Firebase's own SDK** (firebase-messaging 26 directly): +2.15 MB, 51,250 method refs, a `FirebaseMessagingService`
  subclass, the same Firebase project and server changes, and the pod no longer sends plain Web Push to it.
- **FCM only**, dropping UnifiedPush: phones without Play services (GrapheneOS, LineageOS, Huawei, ...) would get no
  closed-app push at all, and users who chose ntfy would go through Google anyway.

Decision:

- **UnifiedPush stays; FCM is the fallback.** Registration (`push::android::register`) uses the distributor saved
  before, else the first distributor app installed, else the embedded FCM distributor (this app itself, listed
  because the manifest declares its receivers); on a phone without Play services that registration fails and
  notifications show only while the app runs, as before.
- **Connector 3.3.5**, with the embedded FCM distributor 3.1.0, kotlin-stdlib 2.3.0 and kotlinx-coroutines-core-jvm
  1.10.2 added by the script (Maven Central, pinned by hash). Their manifest entries are copied into ours by hand
  (the script doesn't merge library manifests): the distributor's `FirebaseReceiver` and `UnifiedPushReceiver`, the
  connector's `RaiseToForegroundService`, and the `com.google.android.gms` package query.
- **Keys and decryption stay in Rust.** `PushReceiver` extends connector 3.x's `MessagingReceiver` and overrides
  `getKeyManager` with `PushKeys`, whose five methods (`generate`, `exists`, `getPublicKeySet`, `decrypt`, `delete`)
  each forward to `NativePush` (`src/push.rs`). The connector's Tink-based key store is never built, so Tink,
  protobuf and gson are left out of the APK. Registration passes the pod's VAPID key (`GET /v1/push/key`) and a
  `PushKeys`; the connector calls Rust to decrypt before `onMessage`, which forwards the plaintext.
- Privacy: with the fallback, Google's push service carries messages, encrypted end to end (RFC 8291) as through
  any distributor; it sees their timing and size, and the pod's VAPID key. A distributor app, when installed, is
  always preferred.

Not yet: checked on a phone (with Play services and no ntfy; with ntfy); moving an existing install's connector
2.5.0 registrations (3.x migrates its own store, Rust's keys are kept); offering to switch to a distributor installed
after the fallback was chosen.
