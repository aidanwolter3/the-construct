# Public links are reviewed, read-only copies of a layout and the Collections rows it shows

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
    only the fields the layout reads (`$item.<f>`, `#<f>`, quoted names); a field the page doesn't show never
    leaves the pod;
  - at most 512 KiB; at most 20 live links per pod.
- **Review**: `share_publicly` without `confirm` answers every text and value a visitor would read, every URL, the
  rows and fields, and what was taken out, and says the copy follows new data. `confirm` must repeat the review's id
  (a hash of the copy): anything that changed asks again.
- **The host** keeps copies in `host.db` `public_copies` (migration `public_copies`) under an unguessable link
  (`pub-` and 32 base32 characters). `GET /s/<link>` is the web client's page titled with the copy's title (for link
  previews; `noindex`), `/s/<link>/view` the copy, `/s/<link>/image` an image proxy for URLs the copy names only.
  A visitor never reaches a pod and gets no cookie.
- **Refresh**: each pod watches the data events of the sources that let data out and, two seconds after they settle, copies its
  live links again. A copy that can't be made (the layout was deleted, now draws other data) keeps the last one.
  New rows the layout shows go out without a new review: the review says so.
- **Revoke**: `unshare_publicly` takes the copy off the host at once (the row stays, marked revoked, for the
  owner's list); a deleted pod takes its copies.
- **The page**: `start_public` draws the copy with no loader (nothing it draws can write), images through its own
  proxy; a link it opens goes to a new tab.

Not yet: a short or chosen URL (`/@handle/slug`); a preview image in link unfurls; rate limits per link;
visitors' replies (an RSVP); sharing data other than Collections'; listing public pages.
