# The Rust Android app receives push through the UnifiedPush connector's receiver, keys and decryption in Rust

Date: 2026-11-07. Related: D52 / [`../notifications.md`](../notifications.md),
[android-native-bridge](android-native-bridge.md),
[android-push-registration-per-server](android-push-registration-per-server.md), issue #168.

The Compose Android app gets the pod's Web Push while closed through the UnifiedPush connector 3.x (`PushService`),
which keeps the Web Push keys and decrypts with Tink, with the embedded FCM distributor as a fallback. The Rust
NativeActivity had none of it: a push receiver is a class Android starts by name, and the developer doesn't want
app code in Java (decision `android-native-bridge`: Java only forwards).

Considered: connector 3.x in the APK (its classes plus Tink, protobuf and the Kotlin library; keys and decryption
in Java libraries), our own receiver speaking the UnifiedPush broadcast protocol from Rust (the connector's job,
redone), polling the pod (also needs a class Android starts), and notifications only while the app runs.

Decision:

- **Connector 2.5.0's `MessagingReceiver`**, whose classes (and the Kotlin standard library they need) the script
  adds to the APK from Maven Central, pinned by hash. `PushReceiver` (`android/java`) extends it and only forwards
  its callbacks (`onNewEndpoint`, `onMessage`, `onUnregistered`, `onRegistrationFailed`) to `NativePush`, whose
  native methods are in `src/push.rs`. 2.x hands the app the message as it came (still encrypted) and needs nothing
  else; the connector checks each broadcast's token and acknowledges messages.
- **Keys and decryption in Rust** (`src/push.rs`): a P-256 key pair and auth secret per server, made on the device,
  kept in `push.json` in the app's private files; messages decrypted with `web-push-native` (RFC 8291, the pod's
  own crate). One registration per server, named `server:<base url>` as in the Compose app.
- **Registration from Rust** through JNI: with a pod session, the saved endpoint goes to the pod again and the
  connector is asked to register (the distributor saved before, else the first installed one; no dialog). The
  endpoint comes back to the receiver and goes to the pod when that pod is open, else at its next start.
- **Notifications from Rust**: in channel `<app_id>.<channel>` (made the first time), tagged with the
  notification's id; not posted while the app shows that pod in the foreground, where the engine takes the message
  and the bell shows it. A tap opens `ConstructActivity` with `construct://notification/<token>`, a random token
  remembered with the notification (the newest 50); the activity is exported, so a link with a token this app
  didn't give out opens nothing, and one from another pod than the open one only brings the app up. The window then
  opens it as the inbox does (`open_notification`: marked read, its layout and `target_uri`).
- `request_notification_permission` shows Android's prompt while it still can, else the app's notification
  settings.

Not yet: the FCM fallback for phones without a distributor (the embedded FCM distributor needs Google's Firebase
library and Play services; without a distributor the Rust app gets notifications only while it runs); syncing
channel names and groups from the pod's channel list; `local://device#notification_permission`; checked on a
phone.
