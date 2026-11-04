# Comment notifications open the post's whole thread at the comment

Date: 2026-11-03. Related: D34, D47, D52, [list-focus](list-focus.md).

A comment notification linked to `pixelfed://posts/<in_reply_to_id>/comments`. For a reply to a comment that is the
comment, so the app opened "the replies of that comment": one orphan reply, no post, and a comment box that replied
to the wrong status. The comments screen also never showed the post being discussed.

Decision:

- **Links:** a comment or mention that is a reply links to `pixelfed://posts/<post>/comments/<comment>` (the new
  `comment_thread` screen of `layouts/pixelfed/0007.json`: the same screen plus a List `focus` on the comment).
  `<post>` is the top of the thread: the poll walks up `in_reply_to_id` with `GET /api/v1/statuses/{id}` (at most
  10 hops per reply, 40 lookups per poll, shared per parent). If the walk fails, the link uses the parent and the
  screen walks up instead. A reply whose parent is not the post is titled "replied to your comment".
- **Comments data:** `/v1/pixelfed/screens/comments?post_id=<id>` first fetches the status; when it is a reply the
  pod walks up to the post (so links already in inboxes open the full thread too), then reads the post's thread. The
  bundle also carries the post (`post:<id>`, as the post screen's data), so the screen shows it above the comments.
  Query keys use the id asked for. One extra upstream call per screen open (the status), more for old comment links.
- **Likes on comments:** the layout adds a like button per comment through the existing `/v1/pixelfed/posts/like`
  endpoint (Pixelfed favourites work on any status). Pixelfed has no emoji reactions, so only likes; reactions
  stored only in the pod would be invisible to everyone on Pixelfed.

Limits: clients older than schema 12 stay on layout 0006 (or older), which has no `comment_thread` route, so tapping a new
comment notification there only opens the app. A link from an old notification that names a comment still posts new
comments as replies to that comment (the footer replies to `$route.id`).
