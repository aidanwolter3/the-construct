# Posting photos

How a user posts photos with a caption to Pixelfed, and why the client, the engine and `pod-core` stay generic while
doing it. Decisions: D47 (attachments and uploads), D48 (server-side pending creates and data events), D49 (schema 8
vocabulary).

## What the user sees

1. In the feed, the "add photo" button opens the **New post** screen.
2. They pick one to four photos (thumbnails with a remove button) and may write a caption.
3. **Share** closes the screen. The post appears at the top of the feed at once, marked "Posting…", with their own
   photos, even offline.
4. When Pixelfed has the post, the feed reloads by itself and the real post (likes, comments) replaces the
   placeholder.
5. If Pixelfed refuses it, the placeholder says why, with **Retry** and **Discard**.

## Who knows what

| Part | Knows | Doesn't know |
| --- | --- | --- |
| Layout (`layouts/pixelfed_home/0004.json`) | The compose screen, the 4-photo limit, what the placeholder looks like | — |
| Shared UI | `core://MediaPicker`, picking files, `dismiss` | Pixelfed, posts |
| Engine (`client-core`) | Staging files, uploading a create's attachments before its POST, `position`, `event: data` | What the files are for |
| `pod-core` | Storing uploads, giving adapters access, `event: data`, the `resume` hook | Pixelfed, posts |
| Pixelfed adapter | Media upload, processing, `POST /api/v1/statuses`, pending posts in the feed | — |

A second app (say, recipe photos) gets the same picker, attachments and uploads for free and only writes its own
adapter route and layout.

## The flow

```
client                     engine (outbox)              pod-core            pixelfed adapter          Pixelfed
MediaPicker ── pick ──▶ stage_attachment
                        (JPEG, no EXIF, ≤2048 px)
Share ── Create ──────▶ temp post at top of feed
                        outbox row + attachment ids
                        ── POST /v1/uploads ────────▶ store file
                        ◀── {upload_id} ──────────── 
                        ── POST /v1/pixelfed/posts/create {caption, media:[ids], client_id} ──▶
                                                                          claim uploads, save row
                        ◀──────────────────────────────────────────────── 202
                        reload feed: placeholder now
                        comes from the server
                                                                          job: POST /api/v2/media ──▶
                                                                          poll GET /api/v1/media/{id}
                                                                          POST /api/v1/statuses ───▶
                                                     ◀── publish_data ─── done
                        ◀── SSE event: data ────────
                        reload feed: real post
```

### Client
- `core://MediaPicker {field, max_items}` keeps a list of staged attachment ids in the screen's form.
- `EngineHandle.stage_attachment(path)` decodes the image, scales its longest side to at most 2048 px, writes a JPEG
  without metadata (no GPS) under `<storage>/attachments/`, and registers a local entity
  `construct://attachments/<id>` (`url: file://…`, `aspect_ratio`) so Carousels can draw it.
- `action.mutation.create` gains `attachments` (`{"photos": "$form.photos"}`), `position` and `dismiss`. In the
  intent's `entity`, `$attachments.photos` becomes the local entity URIs; in its `payload`, the upload ids (filled in
  at flush time).
- Store v5 adds the `attachments` table; a create's attachment refs (and their upload ids) live in its
  `create_json`.
- The outbox uploads a row's attachments first (`POST /v1/uploads`, raw body, `Idempotency-Key: <attachment id>`),
  remembers each `upload_id` so retries don't upload twice, then POSTs the payload as for any create. Attachment files
  are deleted when the row is.
- `event: data` on the workspace's SSE stream makes the engine reload the workspace and its loaded data.

### Pod
- `POST /v1/uploads` (signed-in callers; jpeg/png/webp checked by magic bytes; `CONSTRUCT_UPLOAD_MAX_BYTES`, default
  20 MB) stores the file in the pod's own database (`uploads`, deleted with the pod) and answers `{upload_id}`.
- `GET /v1/uploads/{id}` is a public capability URL (like the media proxy), so placeholders can show the photos on any
  device. Uploads nobody claimed are deleted after 24 h; claimed ones when their owner releases them.
- Adapters use `AppState::uploads()` (`get`, `claim`, `release`, `public_url`) and `AppState::publish_data`.
- `AppAdapter::resume` runs when a pod opens, so interrupted jobs start again.

### Pixelfed adapter
- `POST /v1/pixelfed/posts/create {caption, media, client_id}` checks 1–4 media and the caption length (500,
  Pixelfed's default), claims the
  uploads, saves a `pixelfed_pending_posts` row (one per `client_id`) and answers 202. A job then:
  1. uploads each photo (`POST /api/v2/media`; `/api/v1/media` when v2 is missing),
  2. waits for processing (`GET /api/v1/media/{id}` until it has a URL),
  3. posts (`POST /api/v1/statuses` with `media_ids[]`, `Idempotency-Key: construct-<client_id>`),
  4. releases the uploads and publishes `event: data`.
- Transient upstream errors retry with backoff; a refusal marks the row failed with a readable message.
- The home feed lists this session's pending and failed posts first, as `pixelfed://posts/pending-<client_id>` (the
  URI the client's placeholder already has, so nothing flickers). A placeholder has no `id` (the layout hides the
  like/comment row with a `Conditional` on `$item.id`), and either `pending_label` ("Posting…") or `failed: true`
  with `error`. A finished post that the timeline doesn't list yet is fetched by id and shown first; its row is kept
  10 minutes, so a late retry of the create still answers 202.
- `POST /v1/pixelfed/posts/pending/retry` and `/discard` (`{client_id}`) back the placeholder's buttons.

## States of a post

| State | Where | Feed shows |
| --- | --- | --- |
| queued | client outbox (maybe offline) | placeholder, "Posting…" |
| posting | adapter row, job running | placeholder from the server, "Posting…" |
| failed | adapter row | "Couldn't post: <reason>", Retry, Discard |
| done | adapter row until the timeline lists it | the real post |

A refusal before the adapter accepts (a 4xx on upload or create, rare since the layout enforces the limits) drops the
placeholder, like any refused create (D35).

## Later
- Alt text per photo, visibility, content warnings: a field per photo and chips in the layout, plus the adapter
  passing them on.
- Video.
- A post (and its uploads) blocks the outbox queue while it uploads; likes made meanwhile wait.
