# A layout's own sign-in on another server shows its host first

Date: 2026-11-21. Related: [device-owned-local-state](device-owned-local-state.md), D19, D45.

Decision `device-owned-local-state` made `action.device` `sign_in` on another server take only a server the user
typed, and show its host in the app's own dialog before the browser opens. A layout could still skip that guard by
sending the intent `sign_in` wraps, `action.auth.login`, itself, with any `instance_url` (a literal, or a value
bound from row data). The client passed it to the engine as is: when the pod allows other servers, the browser
went straight to a sign-in page of the layout's choosing, and the account signed in there was attached to the
session.

Decision:

- `local://providers` rows carry `server`: the `instance_url` of the provider's own sign-in intent (the pod's
  default server, from `GET /v1/auth/sign-ins`). Layouts can't write them (they are device-owned).
- **`action.auth.login` on that server** (or with no `instance_url`) starts at once, as before.
- **On any other server**, the app shows "Sign in on <host>?" in its own dialog first, the same one `sign_in`
  shows, and its OK (a nonce only the dialog holds) starts the sign-in. A layout's `action.sequence` can't answer
  it. A server the user types into `core:pixelfed_sign_in`'s box gets this dialog too, as it does in Settings.
- A client that can't sign in (`Loader::signs_in` false) answers `NotSent` at once, without asking.

A provider that is already signed in isn't in `local://providers`, so signing in to it again shows the dialog: a
rare case, and the dialog only asks.

Needs a client build: older clients keep the old behaviour.
