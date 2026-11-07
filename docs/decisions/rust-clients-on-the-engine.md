# The Rust clients run on the engine

Date: 2026-11-07. Related: [rust-render-engine](rust-render-engine.md), [layouts-not-apps](layouts-not-apps.md),
D11, D14, D19, D24, D46, D52. Issue #167.

The Rust clients (`construct-viewer` on desktops and the NativeActivity APK, decision `rust-render-engine`) fetched
everything straight from the pod (`PodClient`): every launch waited for the network and showed nothing offline, a
write made offline (or whose POST failed) was lost, and a change made elsewhere (another device, an agent editing a
layout over MCP, a new notification) showed only once the layout was opened again. The Compose clients get all three
from the engine (`client-core`: the SQLite cache, the outbox, the event stream).

Considered: adding a cache and an outbox to `PodClient` (a second copy of the engine's hardest code, which would
drift from it), and running the engine through its FFI as the Kotlin clients do (pointless in Rust). Chosen: the
engine itself behind the Rust clients, as a library.

- `construct_render::engine::PodEngine` (feature `remote`) wraps one `ConstructEngine` per pod and is the session's
  `Loader`. Home, other layouts (`open_layout`) and data paths (`action.data.load`) come from the engine, cached;
  mutations and creates go to its outbox (`Loader::take`); notifications are listed and marked read through it.
- Every snapshot the engine sends becomes `Changes` (data, and the new roots of layouts it fetched again) that the
  session applies on its next frame (`Loader::changes`), the window woken through the session's image store. A
  layout with a new root shows it where it is open; its open screens stay.
- The device data the shared UI builds (`DeviceData`) is built from the engine with the same builders as
  `PodClient`; the layouts list (`GET /v1/apps`, which the engine doesn't cache) is kept in `apps.json` next to the
  engine's cache.
- Sessions live in the engine's `SecureStore`, not in `pod.json` (which keeps the server and the last pod code):
  the login Keychain on macOS, a file only the user can read on other desktops (`FileStore`), and on Android the main
  app's own `KeystoreSecureStore` format through JNI (no Java or Kotlin is written for the Rust clients), so the main
  app's sign-in carries over. A token an older build kept in `pod.json` is moved there once.
- `pods` keeps one engine per pod across sign-in, Leave pod and Try again: signing in, leaving and reloading go
  through the engine, and the session is rebuilt from its snapshot.

`PodClient` stays for `pod_shots`, which compares a live session with the simulator's live mode. Nothing on the wire
changes and no client update is needed.

Not yet: the Rust clients don't run upstream sign-ins (`action.auth.login`), push notifications or a pinned
version's layout (a pin shows in the version list only), and a layout created elsewhere joins home's list only when
home or a notification count changes, or on Try again.
