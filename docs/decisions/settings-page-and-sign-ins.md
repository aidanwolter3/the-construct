# Settings page for the pod, sign-ins and notifications; the pod lists sign-ins

Date: 2026-11-03. Related: D33, D40, D46, D52.

- **Why:** the drawer mixed apps with plumbing: "Signed in as @x" (the Pixelfed sign-in) and "Your pod is open on
  this device" (the pod token, D46) sat together under Account and read as the same thing, next to Connection and
  Server. Users also could not see which Pixelfed server they were signed in to, and a device that opened a pod
  where Pixelfed was already signed in said "Not signed in" (the engine only knows accounts it signed in itself).
Decision:

- **Drawer:** the apps, then a **Settings** row pinned to the bottom (`menu-settings`) with the connection status
  beside it ("Online", "Offline · 2 pending", "Connecting…"). The hamburger badge is unchanged.
- **Settings page** (`settings`, a drawer level like the version picker): "Your pod" (connection and error, the
  platform's server picker, "Pod open on this device" with **Leave pod**), "Sign-ins" (one row per sign-in on the
  pod session: title and "@user · host"; a row opens `sign-in-detail` with server, username, the apps that use it,
  and **Sign out of <title>**), and the Notifications row (D52). Back walks out level by level. With no sign-ins
  listed it shows the old states ("Not signed in" + Sign in, "Signing in…", "Sign-in failed: …"); when the engine
  is signed in but the pod lists none (a pod from before this decision), "Signed in as @x" with Sign out stays as a fallback.
- **Pod:** `GET /v1/auth/sign-ins` (pod-core): `{"sign_ins": [{id, title, server?, username?, apps: [{id, title}]}]}`
  for the caller's session (no bearer: empty; stale: 401). `SignInProvider` gains a `title`; adapters describe the
  account behind a sign-in with `AppAdapter::sign_in_account` (stored data only, no upstream call; default none).
  Pixelfed answers with the session's instance URL and stored username. `apps` follows `GET /v1/apps` (dev apps
  only when shown). Read-only; no migration.
- **Engine:** `ConstructEngine::sign_ins()` is a catalog call (blocking, bearer sent, a 401 never signs out);
  without a session it answers `{"sign_ins":[]}` locally, and a 404 is `SIGN_INS_UNSUPPORTED`, which the client
  treats as an empty list. FFI `sign_ins`; bridge `signIns()` (default: none). The list reloads when Settings opens
  and whenever the auth state changes.
- **Sign out** still calls `logout`, which ends every sign-in on the session (a pod token's session is kept, D46).
  With one provider that is "Sign out of Pixelfed". No `SCHEMA_VERSION` bump; clients need a new build for the new
  menu; old clients keep working against the new pod.
- **Weighed:** putting the account list in the engine's `auth_json` (it is pushed on every snapshot and only knows
  this device's sign-ins); a full-screen Settings outside the drawer (the drawer already handles levels, Back and
  rotation).
- **Since then:** adding a sign-in from Settings landed with custom apps (decision
  [custom-apps-and-data-sources](custom-apps-and-data-sources.md): `providers` in `GET /v1/auth/sign-ins`).
- **Not yet:** signing out of one provider while keeping others (needs a per-provider pod endpoint and engine auth
  state per provider; moot until a second provider exists); avatars and display names in the list.
