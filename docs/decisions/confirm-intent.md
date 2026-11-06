# Layouts ask before an irreversible step with action.confirm

Date: 2026-11-05. Related: [blueprints-and-home](blueprints-and-home.md), [layouts-not-apps](layouts-not-apps.md),
[composable-actions](composable-actions.md), [layout-features](layout-features.md), issue #132.

With home and Settings drawn from layouts, `action.device` `leave_pod` and `sign_out` (and any delete) ran on one tap:
the native menu that used to stand in front of them is gone, and agents put these buttons anywhere. Considered:
drawing the question as a layout (a Sheet with two buttons; rejected: every layout would need one, and an agent's
dialog would look different each time) and a confirm flag on the commands themselves (rejected: deletes are
mutations, and the wording belongs to the layout).

Decision: the intent `action.confirm {title, message?, confirm_label, then}`, layout feature `action_confirm`.

- **The app draws the dialog**, not a layout, so it always looks like the platform's own prompt (a Material
  `AlertDialog`): the layout's title and message, its `confirm_label` button and Cancel. `then` (any intent) runs
  only on confirm; Cancel, Back or a tap outside do nothing.
- **In a sequence**, the steps after a confirm wait for its answer too (they become part of its `then`). The whole
  intent is bound once when tapped (`$item`, `$form`, one `$nonce`). One dialog at a time: a confirm tapped while
  another is open is ignored.
- **Client only:** nothing reaches the engine or the pod until `then` runs.
- **Pod checks:** `title` and `confirm_label` must not be blank; `then` is checked like any other intent. The MCP
  instructions and vocabulary tell agents to wrap `leave_pod`, `sign_out` and deletes in it.
- **Settings** 0005 asks before Sign out and Leave pod. **Older clients** don't send `action_confirm`, so they get
  Settings 0004.
