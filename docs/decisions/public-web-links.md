# Public links are reviewed, read-only copies of a layout and the Collections rows it shows

Extended by [public-links-share-the-closure](public-links-share-the-closure.md): a copy carries the layouts its
root opens and embeds, takes `params`, keeps only the rows each list's `where` shows, and is versioned. Amended by
[public-links-decide-ages](public-links-decide-ages.md): a list's `older_than` is decided on the pod's clock.
Amended by [public-links-visitor-sets](public-links-visitor-sets.md): a row field the copy's own
`action.local.set` changes is the visitor's only for the rows and values that set reaches.

Date: 2026-11-17. Related: [sharing](sharing.md), [web-client](web-client.md), [screen-sizes](screen-sizes.md),
[collections](collections.md).

People want to send someone without a pod a link to something they made: a recipe, an event. Decision `sharing`
lets pods share a layout's shape with other pods but deliberately takes the data out, and nobody without a pod can
look at it. This is the other case: the shape *and* the data, for a person with only a browser.

Considered:

- **Live reads**: the visitor's page asks the owner's pod through a capability scoped to the layout. Always
  current, but visitors' traffic reaches the pod: it would have to scope a client-supplied `bind`, lookups into other
  collections, computed and model fields (on the shared model key) and upstream fetches, and the pod's id would show
  in image URLs. It goes against "privacy is storage" (decision `sharing`). Rejected.
- **A new visibility on the shelf's publications** (`publish` with `web`). Publications are templates whose data
  references become inputs; a public page is the opposite, the data itself. Rejected: a separate table.
- **A copy on the host, refreshed by the pod.** Taken.

Decision: **a public link is a copy, on the host, of a layout as the owner's device draws it and of the Collections
rows it shows, made only after the owner reviews it, read-only, refreshed by the pod when its data changes, and
served to anyone with the link by the web client. Only where the host allows it (`CONSTRUCT_PUBLIC_WEB=1`, with
`CONSTRUCT_WEB=1`).**

- **The copy** (`pod_core::public_copy`): the layout's plumbing and data as `render_layout_preview` reads them
  (components expanded, the owner's data), then:
  - **taken out**: controls whose intent writes, loads or goes elsewhere (a tappable keeps its content; a button
    goes), embeds of other layouts, screens. Kept intents: `action.navigation.back`, `action.local.set` and
    `action.link.open` (its URL is in the review);
  - **only data its source lets out**: each source says which of its data a public link may carry
    (`Source::public_data`, URI and query prefixes; none by default, so the core names no source). Collections
    lets its rows out; a layout that draws anything else (Pixelfed, Events) is refused. The theme
    (`core://theme`, colours) goes too;
  - **only what shows**: rows the copy's lists hold (or only `rows`, or the `item` drawn as `$item`), each with
    only the fields the layout reads (`$item.<f>`, `${<uri>#<f>}`, and the names under a list's or count's
    `where`, `filter`, `group_by`, `indent` and a carousel's `aspect_field`; a plumbing key such as `label` is
    not a field read); a field the page doesn't show never leaves the pod;
  - **only what the owner's screen shows** (issue #446): a `core://Conditional` and an embed's `show_when` filter
    too. A row whose template (through its conditions) is under a condition false for it stays home, and so
    does it through a count of the same list; a field read only under such a condition stays home; what a
    condition tests goes, so the page decides it the same way. A condition that can't be decided when the copy is
    made (a `date` test, a `where` `older_than`, a name left unbound) fails closed: its rows and fields stay home.
    A condition on what the visitor picks (`$form`, a `local://` entity such as the device's size, a row field the
    copy's own `action.local.set` changes) shows, as it does on the owner's screen a tap away; a `where` on a form
    value starts blank and matches every row, as on the owner's screen;
  - at most 512 KiB (2 MiB since decision [public-link-size](public-link-size.md)); at most 20 live links per pod (the shelf counts and inserts in one transaction, so shares at once can't pass it).
- **Review**: `share_publicly` without `confirm` answers every text and value a visitor would read, every URL, the
  rows and fields, and what was taken out, and says how the copy follows new data. `confirm` must repeat the review's id
  (a hash of the copy): anything that changed asks again.
- **The host** keeps copies in `host.db` `public_copies` (migration `public_copies`) under an unguessable link
  (`pub-` and 32 base32 characters). `GET /s/<link>` is the web client's page titled with the copy's title (for link
  previews; `noindex`), `/s/<link>/view` the copy, `/s/<link>/image` an image proxy for URLs the copy names only.
  A visitor never reaches a pod and gets no cookie.
- **Refresh**: each pod watches the data events of the sources that let data out and, two seconds after they settle, copies its
  live links again. A copy that can't be made (the layout was deleted, now draws other data) keeps the last one.
  Edits and removals of the copy's rows go out; new rows (and fields no row of the copy had) wait until the owner
  shares it again, with a new review (issue #446: a private row added later went public on its own).
  `list_public_links` says how many wait (`new_rows_waiting`). `share_publicly {new_rows: "publish"}` lets a link's
  new rows out without a review (kept in its key as `new_rows:publish`, not part of what makes the link); the
  review says which one applies.
- **Revoke**: `unshare_publicly` takes the copy off the host at once (the row stays, marked revoked, for the
  owner's list, without its title or the rows it asked for: migration `public_copies_revoked_forget` cleared the
  ones revoked before; a pod keeps its newest 20 revoked rows, older ones are deleted); a deleted pod takes its
  copies.
- **The page**: `start_public` draws the copy with no loader (nothing it draws can write), images through its own
  proxy; a link it opens goes to a new tab.

Not yet: a short or chosen URL (`/@handle/slug`); a preview image in link unfurls; rate limits per link;
visitors' replies (an RSVP); sharing data other than Collections'; listing public pages.
