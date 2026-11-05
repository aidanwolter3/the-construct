# The Pixelfed app

`crates/app-pixelfed` adapts a [Pixelfed](https://pixelfed.org) instance (D17, D40): it owns the upstream tokens,
turns the home timeline into entities, and serves the `pixelfed` app's data. Its layouts are in
`crates/app-pixelfed/layouts/pixelfed/`. The client holds only a pod session.

Features: the home feed as a list or as two-column cards, photo carousels, likes and bookmarks, threaded comments
with replies and likes, a post screen, [posting photos](posting.md), and [notifications](notifications.md) for likes,
comments, mentions, follows and boosts.

## Signing in

The pod registers its own OAuth app on the instance on the first sign-in (`POST /api/v1/apps`); nothing has to be
registered by hand. The engine opens the instance's authorize page in the browser, receives the redirect, and hands
the code to the pod, which exchanges it and keeps the tokens (sealed with `CONSTRUCT_SECRETS_KEY`). On an upstream
401 the pod refreshes the token once and retries.

- **Which instances:** `PIXELFED_INSTANCE_URL` is the default ("Sign in with Pixelfed"). `PIXELFED_ALLOWED_INSTANCES`
  lists others, or `*` for any public `https://` instance typed on the sign-in screen (D45).
- **Which accounts:** the first account to sign in to an instance is pinned as the pod's owner; another gets "This
  pod belongs to a different Pixelfed account." `PIXELFED_OWNER_ACCOUNT_ID` or `PIXELFED_ALLOW_ACCOUNT_CHANGE=1`
  change that.
- **Redirects:** `http://127.0.0.1:<port>/callback` and `http://localhost:<port>/callback` (desktop),
  `construct://oauth` (Android), and any `PIXELFED_EXTRA_REDIRECTS` entry such as the Android App Link
  `https://<host>/oauth/android`.

### On staging

Open your pod (enter your pod code or tap **Try a temporary pod**). In an admin pod, type your server (e.g.
`pixelfed.social`) under "Or sign in to another Pixelfed server" and approve in the browser. Temporary pods can only
use the staging fake.

### On your own server, with a phone

1. Start a pod for your instance:

   ```bash
   PIXELFED_INSTANCE_URL=https://<instance> CONSTRUCT_DATA_DIR=~/.construct-pixelfed cargo run -p server
   ```

   Add `PIXELFED_MEDIA_HOSTS=<cdn host>` if the instance serves media from another host.
2. Make it reachable from the phone. Simplest is USB: `adb reverse tcp:8080 tcp:8080` and the server
   `http://127.0.0.1:8080`. Over a LAN (`CONSTRUCT_ADDR=0.0.0.0:8080`) the open `dev` pod is refused, so create a pod
   with `server pods create` and enter its code; images need `adb reverse` or HTTPS, since Android allows cleartext
   only to local addresses.
3. Install the app (`./gradlew :androidApp:installDebug`) and pick **Local USB** in Settings' server picker, or start
   a debug build with `--es baseUrl http://127.0.0.1:8080`.
4. Tap **Sign in with Pixelfed**, log in and approve in the browser; the `construct://oauth` redirect returns to the
   app and the feed loads.

Pixelfed throttles `/oauth/*` to 10 requests a minute per address, and each sign-in uses two; repeated attempts can
get 429 for a minute.

## Endpoints

All need the pod session unless marked *public*.

| Call | Answer |
|---|---|
| `POST /v1/auth/pixelfed/app {instance_url, redirect_uri}` | `{client_id, scopes, authorize_endpoint}`; 400 `instance_not_allowed` or `redirect_uri_not_allowed`. |
| `POST /v1/auth/pixelfed/token {instance_url, code, code_verifier, redirect_uri}` | `{session_token, account}`; 400 `oauth_exchange_failed`, 403 `account_not_owner`. With a valid bearer, the sign-in is added to that session. |
| `GET /v1/workspaces/pixelfed` | The feed bundle (`queries["timeline:home"]`, posts, users and media). Without a sign-in, a signed-out screen with a sign-in button. |
| `POST /v1/pixelfed/posts/like {id, liked}` | `{ok, id, is_liked, like_count, is_bookmarked}`. Works on comments too. |
| `POST /v1/pixelfed/posts/bookmark {id, bookmarked}` | The same shape. |
| `POST /v1/pixelfed/posts/comment {id, text, client_id?}` | `{ok, id, comment}`: replies to a post or comment (1–500 characters). `client_id` becomes the upstream `Idempotency-Key`, so an outbox retry doesn't post twice. |
| `POST /v1/pixelfed/posts/create {caption, media, client_id}` | 202: a post with 1–4 uploaded photos, finished in the background ([posting.md](posting.md)). |
| `POST /v1/pixelfed/posts/pending/retry {client_id}`, `…/discard` | Retry or drop a failed post. |
| `GET /v1/pixelfed/screens/post?id=<id>` | The post screen's data. |
| `GET /v1/pixelfed/screens/comments?post_id=<id>` | The comments screen's data: the post (`post:<id>`) and its thread in order (`comments:<id>`, replies after their parent, depth capped at 4). Given a comment, the pod walks up to its post (decision [`comment-threads-from-notifications`](decisions/comment-threads-from-notifications.md)). Comments posted through the pod in the last 10 minutes are added when Pixelfed doesn't list them yet (it raises a post's reply count in a background job and lists no replies while it is 0). |
| `GET /v1/pixelfed/media/{key}` *public* | The media proxy. Every image URL in a bundle on an allowed origin is rewritten to it; others become `null`. |

Upstream errors: 4xx passes through, 5xx and unreachable become 502. Pixelfed lists only direct replies in a
status's `context`, so the comments screen also fetches the context of each comment that reports more replies (at
most 50 extra calls, 6 at a time; D47).
