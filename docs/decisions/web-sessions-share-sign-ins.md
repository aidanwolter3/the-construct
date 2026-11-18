# A browser opened with the pod code starts with the pod code's sign-ins

Date: 2026-11-18. Related: D40, D46, [web-client](web-client.md), [web-upstream-sign-in](web-upstream-sign-in.md).

Sign-ins belong to a session (D40). The pod code is a pinned session (D46): the phone that typed it holds the
Pixelfed sign-in. Typing the same code in a browser makes a new, ordinary session (decision `web-client`), so the
browser started signed out: the Pixelfed feed came back empty, while Settings' jack pill ("Plugged in · @you", the
pod's account) suggested otherwise. Signing in again from every browser adds no protection: the pod code already
opens everything on the pod.

Considered:

- **The browser uses the pod code's session.** Then logging the browser out would sign the phone out too, and the
  browser would hold a session that never expires. Rejected.
- **Sign-ins per pod, not per session.** A larger change to D40 that every source would follow. Not needed now.
- **Copy the sign-ins when the browser opens the pod.** Taken.

Decision: **`POST /web/session` gives the new session every sign-in of the pod code's session**
(`pod_core::session::share_sign_ins`), with what each source keeps per session through a new hook,
`Source::share_sign_in` (Pixelfed: the server the session signed in to, `pixelfed_session_instance`), in one
transaction.

After that each session is its own: a sign-in or sign-out on the phone doesn't reach a browser already open, and the
browser's Sign out (which ends its session) leaves the phone signed in. A browser opened before this keeps its session
as it was; logging out and typing the code again picks the sign-ins up.
