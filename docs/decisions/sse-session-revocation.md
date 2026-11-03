# Events streams re-check their session, and each token may hold only a few

Date: 2026-11-03. Related: D22, D46, D50, D52, security audit #31 (M1, M2).

`GET /v1/workspaces/{id}/events` checked the caller's session once, on connect. A stream opened with a session
then kept forwarding private `notification` and `data` events after the session ended: after `server pods
rotate` (the documented fix for a leaked pod code), after a logout, and after expiry. Only `pods delete` or a
restart ended it. Streams were also not limited per token, so one token could hold enough open to reach Fly's
connection limit.

Two ways to end a revoked stream were considered:

- **Broadcast a revocation** from rotate, logout and the expiry sweep. `server pods rotate` runs as a separate
  CLI process that writes the pod's database directly, so a broadcast inside the running server never hears it.
  Expiry is a time comparison with no event to broadcast from.
- **Re-check the session from the stream.** One indexed SQLite lookup covers every way a session ends, wherever
  the change was made.

Decision: a stream opened with a session re-checks it (`Store::session_valid`, the same check as on connect)
before forwarding each `notification` or `data` event and on a timer every keepalive interval (15 s). When the
session is no longer valid the stream ends. So nothing private is sent after a revocation, and an idle stream
closes within one interval. A stream opened without a session carries only layout events and is not re-checked.
A store error during the re-check ends the stream; the client reconnects and is checked again.

One session token may hold `DEFAULT_SSE_STREAMS_PER_TOKEN` (8) streams open per pod
(`AppState::with_sse_streams_per_token`). Another one is refused with 429 `{"error": "too_many_streams",
"limit": 8}`; the engine treats it like any other failed connect and retries with backoff. A client keeps one
stream open. The headroom is for a pod token shared by a few devices, and for dead connections the server has
not noticed yet. Streams without a session are not counted: on a hosted server every routed request carries a
pod or session token, and the open pod is for development only.

Not done: a logout made through this server could end its streams at once instead of within 15 s. Since the
per-event check already stops private events, only the open connection lingers, so this was left out. The cap
is per token, not per server. Limiting the total number of streams on a server is part of the rest of M2.
