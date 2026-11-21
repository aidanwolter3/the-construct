# Share, public and calendar link tokens are kept as hashes

Date: 2026-11-21. Related: [sharing](sharing.md), [public-web-links](public-web-links.md),
[ical-feeds](ical-feeds.md). Issue #453.

`host.db` (and so every backup) held share (`share-…`), public (`pub-…`) and calendar (`cal-…`) tokens as they are
used in URLs, so a copy of the database was a set of working links. Session credentials were already kept hashed.
The Pixelfed media proxy key was `sha256(url)`, which anyone who knows a pod id and a media URL can compute to find
out whether that URL is in the pod's feed.

Decision:

- **The host stores `SHA-256(token)` in hex and looks tokens up by hashing what it is given**
  (`pod_core::construct::share::token_id`). The columns keep their names (`publications.link`,
  `publication_objects.link`, `public_copies.link`, `calendar_feeds.token`); they now hold the hash. Migration
  `hash_link_tokens` hashes existing rows in place, so links already given out keep working.
- **A link is shown once**, when it is made: `publish` and `share_publicly` return the token only for a new
  link. Later answers (`list_publications`, `list_public_links`, a new version of a publication) name it by `id`
  (the hash) and don't show a url; the owner revokes by name or by that id (`unshare_publicly` takes the id as well
  as the link), and sharing again after an unshare makes a new link. The id is no link: lookups hash their input, so
  an id opens nothing. Calendar feeds are unchanged for the owner: their pod keeps the token in its own settings and
  shows the url as before; only the host stores a hash. Considered: re-deriving links from a host secret, which
  would not cover the links already given out.
- **An expired temporary pod's pages answer 404 at once.** `public_by_link`, `public_by_slug` and
  `calendar_by_token` check `pods.expires_at` as `by_link` does, instead of waiting for the sweep.
- **Media keys are an HMAC of the URL under a per-pod secret** (`pixelfed_media_secret`, made on first use), so a key
  can't be computed from a URL. The migration forgets the keys made the old way; the next feed load or notification
  poll makes new ones, so a client showing a cached image may see a placeholder until it refreshes.

Not covered: a pod's own `server.db` still holds the calendar tokens it shows its owner and the share links its
adoptions came from, as those are the pod's data.
