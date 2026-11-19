# A public link's short path is `/<handle>/<slug>`, and the `@` form redirects to it

Date: 2026-11-18. Related: [public-links-share-the-closure](public-links-share-the-closure.md),
[public-web-links](public-web-links.md), [sharing](sharing.md).

Short paths were `/@<handle>/<slug>` (decision `public-links-share-the-closure`). The owner wants the links he sends
to read `/aidan/recipes`: the `@` is one more thing to type and say, and chat apps write it as `%40`.

A route for `/{handle}/{slug}` can't simply be added to the host's router: it would also match every two-segment path
the host forwards to a pod (`/v1/construct`, `/v1/jacks`, …), and a path that matches a route but not its method
answers 405 instead of reaching the pod.

Decision: the short path is `/<handle>/<slug>` (with `…/view`, `…/image`). It has no route: the host's fallback asks
`pod_host::web::short_path` before it forwards a request, and that answers only `GET`/`HEAD` of a path whose first
segment is a valid handle and second a valid slug. Every first segment the host or a pod routes is a reserved handle
(`v1`, `web`, `oauth`, `healthz`, `ical`, and a few kept free for later), so a short path and a route never meet. A new
top-level route must add its first segment to `RESERVED_HANDLES`.

- `short_url` (from `share_publicly` and `list_public_links`) answers the new form.
- Links already sent keep working: `/@<handle>/<slug>` (and `/@…/`, `%40`, other case, under `/web`) answers a
  permanent redirect (308) to `/<handle>/<slug>`; `/@…/view` and `/@…/image` still answer, for a page already open.
- Paused sharing (`CONSTRUCT_PUBLIC_WEB` unset) answers 503 at both forms.
- A pod whose handle is now reserved loses its short paths; its `/s/<link>` addresses stay.

Not yet: a link to one row of a shared list (`/<handle>/<slug>/<row>`).
