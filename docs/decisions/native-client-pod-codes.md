# The Rust clients keep pod codes in the secure store, log no values, and refuse remote plain HTTP

Date: 2026-11-07. Related: D11, [rust-render-engine](rust-render-engine.md).

A pod code is the login itself: whoever has it opens the pod. A security audit found that the Rust clients (the
desktop `construct-viewer` and the native Android app, both through `construct_render::pods`) logged every
interaction's effects as JSON, which carried each typed character of the pod code (`form_set`) and the session
token (`signed_in`), to stderr or logcat. They also kept the last pod code of each server in plain `pod.json`,
which on a desktop other users of the machine can read. And they sent the code and session to a server configured
as `http://` on another machine, with only a warning.

Considered: not keeping the code at all (the sign-in screen would come back empty after Leave pod, unlike today);
keeping it in `pod.json` with tighter file permissions (still outside the store meant for secrets, and Android
backups would carry it).

Decision:
- **Pod codes live in the secure store** where the session already is (the Keychain on macOS, the Android keystore,
  else the owner-only `secure.json`), under `pod_code:<server>`. `pod.json` keeps only the server. Codes and a
  session token that an older `pod.json` holds move to the store on the first start and the file is rewritten
  without them.
- **Logs carry event names, never values**: effects and platform events are logged by name (`form_set`,
  `signed_in`, `link`), never with what was typed, a token, a link or a message.
- **Plain `http://` only to this machine or a private network**: the Rust clients refuse to open a pod at
  `http://` unless its host is `localhost`, a loopback address, 10.0.2.2 (the Android emulator's host), a private
  IPv4 address (10/8, 172.16/12, 192.168/16), link-local, or an IPv6 unique local address, where development
  servers run. Any other host name is refused, since what it resolves to can change. A refused configured or saved
  server falls back to staging; `set_server` to one stays on the current pod.

The Compose clients are unchanged (the Android one already refuses cleartext to anything but its loopback hosts;
D11).
