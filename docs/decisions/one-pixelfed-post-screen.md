# One Pixelfed post screen: the whole photo, then its comments

Date: 2026-11-03. Related: D48, D52, [comment-threads-from-notifications](comment-threads-from-notifications.md).

The post screen (grid cards) and the comments screen (comment button, notifications) showed the same post twice
over. Neither showed the whole photo: the post screen clamped its aspect ratio to 0.8–1.91 like the feed, and the
comments screen cut the cover to a 2.5:1 strip. Opening a post from the grid also had no comments.

Decision: `layouts/pixelfed/0008.json` has one post screen layout, used by three routes:

- `pixelfed://posts/{id}`: grid cards and the feed's comment button (now a Push, not a Modal) open it; like and
  boost notifications link to it (`notify::post_target`).
- `pixelfed://posts/{id}/comments`: the same screen, kept for links already in inboxes.
- `pixelfed://posts/{id}/comments/{comment}`: the same screen with the List `focus` on the comment.

Each shows the post with the whole first photo (the carousel uses the photo's own aspect ratio, no clamp), its
actions and caption, then the comment thread and the comment box. Data comes from
`/v1/pixelfed/screens/comments`, which already carries the post (`post:<id>`) and the thread.

Layout and pod only; no schema bump. The carousel sizes itself by the first photo, so later photos of a different
shape are still cropped. Clients on layout 0006 or older open a like notification on their post-only screen.
`/v1/pixelfed/screens/post` is no longer used by the newest layout but stays for older ones.
