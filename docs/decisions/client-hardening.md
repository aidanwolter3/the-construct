# Client hardening: clipboard, bounded text and depth, OS keyring

Found by the security audit of 2026-11-21 (issue #459). Four small client-side gaps, closed together.

- **The clipboard is the user's.** `edit.paste`, `edit.copy` and `edit.cut` run only for the user's own gesture on
  the edit menu or for the platform's keyboard (`Session::dispatch`), never from a layout's tap, gesture or swipe
  (`Session::clipboard_ok`). A layout can't read the clipboard into a field (where it could be bound into an image
  URL) or overwrite it. Paste still goes only into the focused field.
- **Text is bounded at render time.** One `core://Text` draws at most `MAX_TEXT_CHARS` (20,000) characters, cut with
  an ellipsis; the rest is never shaped, so one huge text can't slow every layout pass.
- **Depth is checked before building.** The pod caps each file and component nesting, but expanded trees can run
  about a thousand deep, and the layout engine recurses per level. `SceneBuilder::relayout` first measures the tree
  without recursion (`UiNode::depth`) and refuses one deeper than `MAX_BUILD_DEPTH` (256); the builder also leaves
  out any node below that depth (inline layouts), so one layout can't crash the app on every start.
- **The desktop session goes in the OS credential store.** macOS keeps the login Keychain; Windows uses the
  Credential Manager and Linux the Secret Service (`keyring` crate), with the private `secure.json` as the
  fallback where there is none (a Linux box without a Secret Service). A session an earlier build left in the file
  moves into the keyring the next time it is read.

Not done: Android's `pod.json`, `cache.db` and push keys stay unencrypted (backups are off and the session is in
the Keystore); the OAuth App Link is already preferred where the pod has one.
