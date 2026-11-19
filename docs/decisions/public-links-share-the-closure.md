# Public links share what a layout reaches, with route params and only the rows that show, and keep working

Short paths superseded by [short-paths-without-at](short-paths-without-at.md).

Date: 2026-11-17. Related: [public-web-links](public-web-links.md), [sharing](sharing.md),
[web-client](web-client.md), [web-on-staging](web-on-staging.md). Issue #363.

Sharing one recipe from a real pod showed four gaps in decision `public-web-links`:

- **One screen only.** Every open of another layout was taken out, so a public link to a list of meals showed the
  meals but tapping one did nothing. Owners expect sharing an element to share what it reaches, as `publish`
  already does for templates (decision `sharing`: the closure of what a name embeds, links to and uses).
- **No route params.** A recipe screen reading `$route.id` drew empty: `share_publicly` had `item` but nothing for
  `$route`. The owner had to copy the screen with the meal hard-coded.
- **Rows the screen doesn't show.** The copy carried every row of each list's query and ignored the list's
  `where`, so a recipe's copy held every recipe's lines and steps and was refused as too large, against "only what
  shows".
- **Not durable.** Links live in `host.db` on one machine's volume with no stated backup, die with a host switch,
  and nothing promised a later format change keeps them working.

Considered for the closure: copying each pushed layout once per row as separate pages (simple, but a list of 30
meals would be 31 copies to review and refresh); resolving opens live against the pod (rejected in
`public-web-links`: visitors would reach the pod). Taken: **one copy holds the root and every layout it reaches,
each once, keyed by layout id**; the page opens them from the copy.

Decision:

- **The closure** (`pod_core::public_copy::make`): from the shared layout, every layout it opens
  (`action.navigation.open` with a layout and no screen) or embeds (`core://Layout`), and what those reach, is
  copied the way the root is (as the owner's device draws it, then stripped), at most 16 layouts. Opens and embeds
  of a carried layout stay; the rest go as before. A layout isn't carried when its data may not go out (Pixelfed,
  Events: refused for the root, left out when reached, and its openers go), when it is a built-in layout (Settings,
  the inbox), or when the owner names it in `leave_out`.
- **Params**: `share_publicly` takes `params` (`{name: value}`, as `render_layout_preview`): the root draws them as
  `$route.<name>`. A layout pushed from a row gets that row's `params` (`{"id": "$item.id"}` bound to each row).
  Params are part of a link's identity (with `rows` and `item`; other params are another link) and of the review
  id; refresh draws with the saved ones. They are kept in the stored key (`public_copies.rows`, as
  `param:<name>=<value>`, beside `item:` and `leave_out:`), so no column changed for them.
- **Only rows that show** (`Shows`): the copy draws its layouts in the head, once per use (the root with its params
  and item; each carried layout with each row that pushes it, bounded at 2000 uses): each list (and count, dock,
  chips) keeps the rows its `where` lets through with that use's `$route` and `$item`, as the renderer tests them
  (`equals`, `not`; a missing field doesn't match). A condition it can't decide (an age, a form value) keeps its
  rows. Rows a `${<uri>#<field>}` names (with `$route` and `$item` bound, so a line's
  `${collections://ingredients/$item.ingredient#staple}`) go with only that field. Each row carries only the fields
  read where it shows. Rows a list's query holds but its `where` hides never leave the pod. `rows` still limits the
  root's lists.
- **Review**: per carried layout, its title, texts, URLs, rows and fields, and the layouts not carried with why. The
  review id covers the whole copy.
- **Size**: 512 KiB for the whole copy (2 MiB since decision [public-link-size](public-link-size.md)); over it, the refusal names the three largest layouts (their plumbing and
  rows).
- **The copy's format** is versioned: `format: 2` is `{format, layout, title, node, data, item?, params?, layouts?}`;
  a copy without `format` is format 1 (`{title, node, data, item?}`). `construct_render::public::session` draws
  every format a host has served, and the web client uses it, so the server and its page ship together. A change
  to the copy is a new format number that the page also reads; stored copies are rewritten by the next refresh
  (each pod copies its live links again when their data changes), never dropped. A link made before this decision
  opens unchanged until then.
- **The page**: public mode puts the copy's layouts in the session by key, so a tap pushes one with the row's
  params and Back (Escape, or a back button) returns, with no loader and nothing that writes.
- **Short paths**: `share_publicly {slug}` also answers the copy at `/@<handle>/<slug>` (the pod's handle, decision
  `sharing`; one live link per pod and slug, column `slug`, migration `public_copies_slug`). A pod can't change its
  handle while a live link has a slug, as with publications. The `/s/<link>` address stays.
- **Stable addresses**: links are answered with the host's `CONSTRUCT_PUBLIC_URL` when set, else the request's
  host. A domain move keeps old links working as long as the old host name still reaches the server (a second
  certificate on the same app); paths never change.
- **Paused, not gone**: with `CONSTRUCT_PUBLIC_WEB` unset, `/s/…` and `/@…` answer 503 "Sharing is paused on this
  server" and the stored copies stay; setting it again brings every link back.
- **Backups**: staging's volume (every pod's database and `host.db`) is snapshotted daily by Fly, kept 5 days (Fly's
  defaults, spelled out in `deploy/fly/pod.toml`; restoring is a new volume from a snapshot). `server backup <dir>`
  makes a consistent copy (`VACUUM INTO`) of `host.db` and each pod's `server.db` while the server runs; a backup
  folder put at `<data dir>/restore` replaces the data on the next start, the replaced data kept aside. Both are in
  `docs/server.md`. No copy leaves Fly automatically: that needs object storage (a new paid service and a secret),
  which waits for the developer's go-ahead.

Not yet: an automatic off-site copy (above); per-row copies of a screen's `screens` (they are still taken out);
`filter` and `group_by` conditions applied (their rows go, the page filters them); a redirect from an old host
name to a new one; a preview image in link unfurls.
