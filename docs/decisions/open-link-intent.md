# Layouts open web links with action.link.open

Date: 2026-11-03. Related: D34, [layout-features](layout-features.md),
[composable-actions](composable-actions.md), [watcher-rules-per-site](watcher-rules-per-site.md), issue #63.

No intent could leave the app: the Watcher's event screen showed a tickets URL as text the user couldn't tap.
Considered: making `core://Text` turn URLs into links (rejected: every text would need parsing on every draw, and
long URLs make poor labels) and a link node (rejected: a button or tappable row that runs an intent already exists
everywhere).

Decision: the intent `action.link.open {url}`, layout feature `open_link`.

- **Client only:** the shared UI opens the link with the platform's browser (Compose `LocalUriHandler`: the desktop
  browser, an Android `ACTION_VIEW`); nothing reaches the engine or the pod. `url` is usually a binding
  (`$item.tickets_url`); after binding, only `http://` and `https://` open; a blank binding or another scheme does
  nothing (logged).
- **Pod checks:** in a custom app, a literal `url` must be `http(s)://` or a binding.
- **Simulator:** no browser opens; `assert-link <text>` checks what a tap opened.
- **Older clients** don't send `open_link`, so they get the newest layout version without it (the Watcher's 0001).

Not done yet: in-app browser tabs (Custom Tabs / SFSafariViewController); a confirmation before leaving the app.
