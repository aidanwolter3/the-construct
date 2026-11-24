# Recent server errors: each pod keeps its own last failed requests

Date: 2026-11-24. Related: [server-monitoring](server-monitoring.md), [device-versions](device-versions.md), D46.

A phone's outbox retried one save for hours because the pod answered it 502 every minute. Server health only said
"Server errors: 3 in the last hour" and the counter `construct_http_responses_total{status="502"}`: nothing about
which request failed, for which device, or why. The counters are process-wide and labelled only by status (their
labels are fixed strings, never request data, so the series stay few), so they can't say more.

Considered: labelling the counter by route (still no device or code, and a pod's routes would show in every pod
with monitoring on); logging failures (the operator reads logs, the pod's owner doesn't); a list in the host's
`count_responses` (the host only sees the raw path, which can hold a token, `/v1/p/<pod>/...`, and the host-wide
list would mix pods).

Decision: every pod keeps its own last 50 answers of 400 or more (`pod_core::recent_errors`), in memory
(`Pod::local`, so no other pod's state can hold them), recorded by the outermost of the pod router's route
layers, where axum's `MatchedPath` gives the route's pattern. Each keeps the time, method, route pattern, status,
the answer's `error` when the answer is a JSON object of known size up to 4 KiB (a body already in memory; a
streamed one is never read) and is one word, and the device id the request named. Never the path asked for, the
query, a body, a header value, a bearer or a pod code. 401 (a stale session, routine) and 404 are left out;
requests the host answers before any pod (wrong pod codes, unknown tokens) belong to no pod and stay counted only.

They are the pod's own data, scoped like its devices (the most private per-pod data the `monitor` source shows):
the `monitor` source's `errors` feed (`monitor:recent_errors`) lists them in any pod with no operator switch, and
nowhere else. In a pod with monitoring on, the `server_errors` check (whose count stays server-wide) adds the
pod's own most frequent 5xx of the last hour, with the device's name: `· in this pod: POST /v1/taste/react → 502
taste_unreachable ×3 (Pixel 10 Pro)`. Server health (layout version 0002) lists them under "Recent failed
requests".

Limits: the list lives as long as the pod's open state, so it restarts with the server, like the counters; it says what failed, not the upstream's own message (the `detail` text is never kept, as it can quote upstream
data).
