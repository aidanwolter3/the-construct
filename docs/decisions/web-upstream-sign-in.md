# The web client signs in to upstreams by sending the tab to the authorize page and back to `/web`

Date: 2026-11-17. Related: D19, D45, D46, [web-client](web-client.md),
[android-app-link-sign-in-redirect](android-app-link-sign-in-redirect.md).

In a browser, Settings' **Sign in to Pixelfed** did nothing: the web client left upstream sign-ins out (decision
`web-client`, "Not yet"), so its loader refused `action.auth.login` and Settings' `sign_in`, and nothing in a web pod
could reach Pixelfed. The native clients sign in through the engine (`client_core`), whose flow is blocking HTTP and a
loopback listener or an app link; none of it runs in a browser.

Considered:

- **A popup** for the authorize page that posts the code back to the opener. Popups get blocked, and a second window
  has to be found again and closed; nothing it gives is needed.
- **A redirect page of its own on the pod** (`/web/oauth`) that hands the code to the client. One more route and page
  for what the client's own page can do.
- **The tab goes to the authorize page and comes back to `/web`.** Taken.

Decision: **the web client's redirect is its own page, `<origin>/web`.**

- Settings' `sign_in` (the provider's intent from `GET /v1/auth/sign-ins`, on a typed server when the provider takes
  one) and a layout's `action.auth.login` register `<origin>/web` with the pod (`POST /v1/auth/pixelfed/app`), make a
  PKCE verifier (S256) and a `state` from the browser's CSPRNG, keep them with the instance and token path in the
  tab's `sessionStorage` (`construct.sign_in`) and send the tab to the authorize page.
- The instance sends the tab back to `/web?code=…&state=…` (or `error=…`). Before it draws, the client takes the
  query off the address (`history.replaceState`), drops the waiting sign-in, and only if its `state` matches and it
  is under 15 minutes old trades the code at the pod (`POST /v1/auth/pixelfed/token`). The request carries the
  browser's session cookie, so the pod adds the sign-in to that session (D40). Progress and errors are
  `local://session#status`, the line Settings shows under Sign-ins.
- **The pod accepts the redirect only as an exact `PIXELFED_EXTRA_REDIRECTS` entry**, as it does the Android App Link
  and for the same reason (a rule built from the request's `Host` would let a proxy that passes any `Host` register
  the pod's upstream app for someone else's page). Staging lists `https://the-construct-pod.fly.dev/web`, and its fake
  Pixelfed the same in `FAKE_PIXELFED_EXTRA_REDIRECTS`; a server that serves `/web` and wants browsers to sign in
  lists its own. Without it the status reads "This pod doesn't accept sign-ins from the web yet."
- The client now sends `auth_login` in its features, so the pod serves the newest versions of layouts that use it.
- **Sign out** ends the browser's session (`POST /web/logout`, then the code form), as `POST /v1/auth/logout` does any
  session that isn't a pod code's: a browser's sign-ins live on its session.

Not yet: signing out of one upstream while keeping the browser's session (the pod has no per-sign-in logout).
