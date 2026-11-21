# The device's own `local://` data is read-only to layouts

Date: 2026-11-21. Related: [blueprints-and-home](blueprints-and-home.md), [layouts-not-apps](layouts-not-apps.md),
issue #434.

Device data (`local://servers`, `session`, `providers`, `theme`...) and the view state layouts write with
`action.local.set` (`local://shell`, `local://collapsible/...`) shared one namespace, and `set_server` read the
server to open from that same map. One tap of an adopted or prompt-injected layout could
`local.set local://servers/x url="https://evil.example"` and then `set_server x`: the app saved that server and
opened it at every start, where it could show its own "enter your pod code" screen. `sign_in` likewise took any
`value` as the server to sign in on, so a button could start an OAuth sign-in on a server the layout chose.

Decision:

- **Device-owned entities** are `protocol::DEVICE_OWNED_LOCAL`, named by the first segment after `local://`:
  `servers`, `session`, `device`, `providers`, `sign_ins`, `connection`, `notifications`, `inbox`, `channels`,
  `theme`, `nav`, `screen`. Layouts read them; `action.local.set` on one is refused by the pod's validator (a
  literal URI) and ignored by the renderer (any URI, once bound). Every other `local://` entity stays the layouts'
  own. Values kept (`local_keep`) for a device-owned entity before this are not put back.
- **`set_server`** looks the id up in the platform's own server list (`Loader::server`, from `PodHome::servers`),
  never in session entities, and the app asks in its own dialog before it switches.
- **`sign_in` on another server** takes `value` only when it is what the user typed into a field of the screen (a
  literal or another binding is ignored), and only for a provider whose `local://providers` row says
  `custom_server`; the app shows the host and asks before opening the browser.
- The dialog's OK runs the session's own `action.device` `answer_asked` with a nonce only that dialog holds; the
  command isn't in `DEVICE_COMMANDS`, so the pod refuses it in a layout, and a layout's `action.sequence` can't
  answer for the user.

Not done: a non-local `action.local.set` (e.g. `$item.uri`) still writes the session's and the engine's copy of
that entity. The pod's validator checks a literal URI's scheme against the layout's sources; a bound one is the
client's, which doesn't know a layout's sources. That is a device-only overlay, never sent to the pod, and the next
load replaces it.
