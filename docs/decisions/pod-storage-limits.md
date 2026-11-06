# Pods get upload quotas, a low-disk guard and stricter temporary-pod rate limits

Date: 2026-11-03. Related: D46, D49, security audit #31 (M2 and the rate-limit Info item).

Anyone can make a temporary pod on staging (D46), and each could hold about 800 MB of unclaimed uploads
(40 per session × 20 MB, D49) for a day on a 1 GB volume. Once the volume is full every pod's SQLite writes fail,
sign-ins and sessions included. Creation was limited per IPv4 address or IPv6 /64, but a /48 holds 65536 /64s,
and the limiter forgot the oldest key once it tracked 10 000, so cycling through addresses reset one's own count.

Decision:

- **Upload quota per pod.** All of a pod's uploads together stay under a quota: 500 MiB for an admin pod
  (`CONSTRUCT_POD_UPLOAD_QUOTA_BYTES`), 25 MiB for a temporary pod (`CONSTRUCT_TEMP_POD_UPLOAD_QUOTA_BYTES`). The
  host sets it on each pod it opens, by the pod's kind; `pod-core` only enforces the number it is given
  (`AppState::with_upload_quota_bytes`) and stays app- and kind-agnostic. Over the quota: 507
  `upload_quota_exceeded`. No new table or column: the quota is a `SUM(size)` over the existing `uploads` table.
- **Low-disk guard.** While the data volume has less than `CONSTRUCT_MIN_FREE_DISK_BYTES` free (100 MiB), uploads
  get 507 `low_disk_space` and `POST /v1/pods/temporary` gets 503 `low_disk_space`. The quotas bound one pod; the
  guard bounds them all, since 50 temporary pods × 25 MiB plus admin pods can still exceed the volume.
- 507 is a 5xx, so clients' outboxes retry it with backoff, as they do 429 `too_many_uploads`: space frees as
  posts finish (released uploads are deleted) and the sweeper removes unclaimed ones. No client change.
- **Temporary pod creation** is limited per IPv4 address or /64 (5 an hour) and also per /48 (10 an hour). A
  request over the /48 limit doesn't count against its /64, so one /48 adds a bounded number of keys.
- **The limiter never drops a live window.** When its table is full it first forgets ended windows; if every
  slot is still live, a new key is refused (fails closed). Filling the table now takes about 1000 /48s or
  10 000 IPv4 addresses, and only blocks new keys until windows end.
- **The `/v1/pods/open` limit stays on that route only.** Any route takes `Bearer cpod_…`, so the limit doesn't
  stop guessing, and doesn't need to: a code is 256 random bits. Limiting failed bearers everywhere would add
  nothing against guessing and would let one client behind a shared address (CGNAT) lock its neighbours out of
  their pods. The limit only keeps the code screen from being used to hammer `host.db`.

Not done here: the SSE stream cap per token (separate change), and an IPv4 /24 level (too many people share one).

**Update (2026-11-06, issue #158): a storage quota per pod.** The other per-pod tables turned out not to be small:
layout versions (50 layouts × 500 versions × 256 KiB, deleted layouts not counted), component versions (200 × 100 × 64
KiB) and collections' undo data could each hold gigabytes, bounded only by the write rate. Rather than a quota per
table, each pod's whole database is held to a size: 768 MiB for an admin pod (`CONSTRUCT_POD_STORAGE_QUOTA_BYTES`), 16
MiB for a temporary pod (`CONSTRUCT_TEMP_POD_STORAGE_QUOTA_BYTES`). It is SQLite's own `max_page_count`, set by the host
on each pod it opens (`Store::set_max_bytes`), so every table counts, uploads included (they are rows), and deletes free
pages for later writes. A write past it fails with `SQLITE_FULL`, which `pod_core::blocking` answers with 507
`pod_storage_full` (clients' outboxes retry 507s with backoff, as for the upload quota). The upload quotas stay as a
sub-limit, so photos can't crowd out an admin pod's layouts and data entirely. Numbers: an admin pod's 500 MiB of
uploads plus room for everything else, under the 1 GB volume; a temporary pod is for trying things, and 16 MiB is far
more than a tried layout and a few collections use (it is below the 25 MiB temporary upload quota, which now rarely
binds). A pod whose database is already larger keeps its size as the limit (SQLite won't shrink it), and its writes that
need new pages are refused until something is deleted. The low-disk guard still covers only uploads and new temporary
pods; with every pod capped, the 50 temporary pods together hold at most 800 MiB, which the 1 GB volume holds next to a
few admin pods' everyday data.
