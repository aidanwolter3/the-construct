# Running the server

`cargo run -p server` starts one server that hosts many pods. This page covers configuration, pods, monitoring and
deployment. The HTTP API is in [api.md](api.md).

## Locally

```bash
cargo run -p server                       # http://127.0.0.1:8080
cargo run -p fake-pixelfed                # optional: a fake Pixelfed upstream on 127.0.0.1:8081
curl -s http://127.0.0.1:8080/v1/workspaces/pixelfed_demo | jq .
```

On a loopback address, callers without a pod token use a pod named `dev` (created if missing), so `curl` and a
local client work without setup. `GET /healthz` answers `ok`. Data lives in `~/.construct-server` unless
`CONSTRUCT_DATA_DIR` says otherwise.

## Configuration

Everything comes from the environment; empty values count as unset. The chosen Pixelfed instance and the host's
settings are logged at startup.

### Network and storage

| Variable | Default | Meaning |
|---|---|---|
| `CONSTRUCT_ADDR` | `127.0.0.1:8080` | Listen address. `0.0.0.0:8080` accepts LAN connections. |
| `CONSTRUCT_DATA_DIR` | `~/.construct-server` | `host.db` (pods, token hashes) and `pods/<id>/server.db` (each pod's data). Created with mode 0700, databases 0600. |
| `CONSTRUCT_PUBLIC_URL` | from each request's `Host` | The base URL clients reach the server at, for the OAuth metadata Claude reads. `https` is assumed behind Fly or with `X-Forwarded-Proto: https`. |
| `CONSTRUCT_SECRETS_KEY` | unset | Seals upstream credentials (Pixelfed tokens, OAuth client secrets) with AES-256-GCM: base64 of 32 random bytes (`openssl rand -base64 32`). Comma-separate keys to rotate: the first seals, all open. Unset: plaintext with a warning (an error on Fly). A sealed row whose key is missing reads as signed out. Production must set it (decision [`upstream-credentials-sealed-at-rest`](decisions/upstream-credentials-sealed-at-rest.md)). |
| `CONSTRUCT_SESSION_TTL_DAYS` | `90` | Lifetime of sessions other than pod tokens (1..=36500). Pod tokens don't expire. |
| `RUST_LOG` | `info` | `tracing` filter, e.g. `server=debug,info`. |

### Pods

| Variable | Default | Meaning |
|---|---|---|
| `CONSTRUCT_OPEN_POD` | `dev` on loopback, else unset | Callers without a pod token use this pod. Development only; `off` disables it. Refused on a non-loopback address (unless the next variable is set) and always on Fly. Only requests whose `Host` is `localhost`, `*.localhost` or an IP address use it, so a DNS-rebinding page can't. |
| `CONSTRUCT_INSECURE_OPEN_POD_ON_NETWORK` | unset | `1` allows the open pod on a non-loopback address: everyone who can reach the server gets that pod and its sign-ins. Only on a network you trust. |
| `CONSTRUCT_TEMP_PODS` | unset | `1`: anyone may create a temporary pod (`POST /v1/pods/temporary`). |
| `CONSTRUCT_TEMP_POD_HOURS` | `24` | Temporary pod lifetime (1..=720). |
| `CONSTRUCT_TEMP_POD_MAX` | `50` | Live temporary pods at most. |

### Storage limits (decision [`pod-storage-limits`](decisions/pod-storage-limits.md))

| Variable | Default | Meaning |
|---|---|---|
| `CONSTRUCT_UPLOAD_MAX_BYTES` | 20 MB | Largest single upload (`POST /v1/uploads`). |
| `CONSTRUCT_POD_UPLOAD_QUOTA_BYTES` | 500 MiB | All of an admin pod's uploads together. |
| `CONSTRUCT_TEMP_POD_UPLOAD_QUOTA_BYTES` | 25 MiB | The same for a temporary pod. |
| `CONSTRUCT_MIN_FREE_DISK_BYTES` | 100 MiB | Below this free space, uploads and new temporary pods are refused, so other writes keep working. |

### Pixelfed

| Variable | Default | Meaning |
|---|---|---|
| `PIXELFED_INSTANCE_URL` | `http://127.0.0.1:8081` (the fake) | The default instance behind "Sign in with Pixelfed". |
| `PIXELFED_PUBLIC_URL` | the instance URL | Base for browser-facing links (the authorize page), e.g. `http://10.0.2.2:8081` for the Android emulator. |
| `PIXELFED_ALLOWED_INSTANCES` | unset | Other instances users may sign in to: comma-separated base URLs, and/or `*` for any public `https://` instance typed on the sign-in screen (only public addresses are contacted). Set: the sign-in screen shows a server box. |
| `PIXELFED_MEDIA_HOSTS` | unset | Extra origins the media proxy may fetch from (a CDN or S3 host). |
| `PIXELFED_EXTRA_REDIRECTS` | unset | Extra OAuth redirect URIs accepted, e.g. `https://<host>/oauth/android` for the Android App Link. |
| `PIXELFED_OWNER_ACCOUNT_ID` | unset | The only account allowed on the default instance. Unset: the first account to sign in is pinned. |
| `PIXELFED_ALLOW_ACCOUNT_CHANGE` | unset | `1`: any account may sign in and replace the stored one (not for `*` instances). |
| `CONSTRUCT_ANDROID_CERT_SHA256` | unset | SHA-256 fingerprints of Android signing certificates allowed to open `https://<host>/oauth/android`, served at `/.well-known/assetlinks.json` (decision [`android-app-link-sign-in-redirect`](decisions/android-app-link-sign-in-redirect.md)). Not secret. |
| `CONSTRUCT_ANDROID_PACKAGE` | `org.theconstruct.android` | The package `assetlinks.json` names. |

### Notifications

| Variable | Default | Meaning |
|---|---|---|
| `CONSTRUCT_PUSH` | on | `off`: no Web Push (notifications still reach the inbox and open apps). |
| `CONSTRUCT_VAPID_PRIVATE_KEY` | generated | Web Push signing key: base64url of a 32-byte P-256 private key. Unset: generated on first start and kept in `host.db`. Set it in production so a lost volume doesn't orphan subscriptions. |
| `CONSTRUCT_VAPID_SUBJECT` | the repository URL | `mailto:` or `https:` contact for push services. |

### Models (Collections)

| Variable | Default | Meaning |
|---|---|---|
| `CONSTRUCT_INFER_API_KEY`, `CONSTRUCT_INFER_MODEL` | unset | Both set: `model`-computed collection fields are answered by this model over the Messages API. Temporary pods never use it. |
| `CONSTRUCT_INFER_URL` | `https://api.anthropic.com/v1/messages` | The Messages API endpoint. |
| `CONSTRUCT_FILL_MODEL` | `claude-opus-5-5` | The model for `agent` and `extract: llm` fillers (same key and endpoint). Without a key those fillers report themselves unavailable. |
| `CONSTRUCT_FILL_AGENT_DAILY` | `20` | Agent filler runs per pod per day. |
| `CONSTRUCT_FILL_PRIVATE_HOSTS` | unset | `1`: fetch fillers may read private addresses (local development only). |

### Monitoring and development

| Variable | Default | Meaning |
|---|---|---|
| `CONSTRUCT_METRICS_ADDR` | unset | Serve Prometheus `GET /metrics` on this separate address. |
| `CONSTRUCT_MONITOR_AUTH_FAILURES_PER_HOUR` | `20` | The monitoring source warns above this many wrong pod codes and unknown tokens an hour. |
| `CONSTRUCT_MONITOR_LLM_TOKENS_PER_DAY` | `1000000` | It warns above this many model tokens a day. |
| `CONSTRUCT_SHOW_DEV_APPS` | unset | `1`: `GET /v1/apps` also lists `pixelfed_demo` (it works either way). |
| `CONSTRUCT_TEST_HOOKS` | unset | `1` enables `POST /v1/test/fault` and local Web Push receivers. Tests only. |

The fake Pixelfed's own variables are in [testing.md](testing.md#fake-pixelfed).

## Pods

A pod is one user's space: their sessions, sign-ins, layout history, custom apps and data, in one SQLite file
(D46). Clients name their pod with a **pod token** (`cpod_…`, 256 random bits). The server stores only its hash; the
code is shown once when the pod is made. Any number of devices can use the same code.

In the app, the "Open your pod" screen takes the code (or, where enabled, **Try a temporary pod**). Signing in to
Pixelfed happens inside the pod, so every device of the pod is signed in. Settings' **Leave pod** forgets the code on
one device.

### Admin commands

Run on the server machine (on Fly: `fly ssh console -a <app> -C "/server pods …"`):

```bash
server pods create <name>                      # prints the pod code once
server pods list
server pods rotate <name>                      # new code; the old one stops working and app sign-ins end
server pods delete <name>                      # stops now; files deleted within 5 minutes
server pods notify-test <name>                 # a test notification to the inbox and the pod's devices
server pods mcp <name> [label]                 # a credential for Claude Code that reaches only /v1/mcp
server pods revoke-mcp <name>                  # disconnect every Claude credential of the pod
server pods monitoring <name> on|off           # let this pod's apps read server-wide monitoring data
server pods adopt <name> <path/to/server.db>   # turn a pre-pods single-user database into a pod
server pods remove-adopted [dir]               # delete the originals adopt left behind
```

Pod codes never go into git or config files; keep yours in a password manager.

### Temporary pods

With `CONSTRUCT_TEMP_PODS=1`, `POST /v1/pods/temporary` makes a pod that is deleted with everything in it after
`CONSTRUCT_TEMP_POD_HOURS`. Temporary pods have every app, but Pixelfed only on the default instance (no typed-in
servers), no model calls and no fetch fillers, and a smaller upload quota. At most 5 are made per client address an
hour. They are how visitors and automated sessions try a deployed server:

```bash
pod=https://the-construct-pod.fly.dev
token=$(curl -fsS -X POST $pod/v1/pods/temporary | jq -r .session_token)
curl -fsS -H "Authorization: Bearer $token" $pod/v1/workspaces/recipes | jq '.queries'
```

## Security defaults

- Every response carries `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `X-Frame-Options: DENY`
  and `Content-Security-Policy: default-src 'none'; frame-ancestors 'none'` unless the route sets its own; behind Fly
  also `Strict-Transport-Security`.
- Opening a pod by code is limited to 10 tries a minute per client address (behind Fly, `Fly-Client-IP`).
- Outbound requests to user-chosen hosts (typed Pixelfed servers, push endpoints, filler URLs) resolve only public
  addresses and follow no redirects into private space (`pod_core::net`).

## Monitoring

The server counts what an operator wants to notice: wrong pod codes and unknown tokens, rate limits hit, credentials
used out of scope, new Claude connections, 5xx responses, temporary pod requests, and model calls and tokens per
feature, plus pods, credentials and free disk (decision [`server-monitoring`](decisions/server-monitoring.md)).
Counts live in memory and restart from zero.

- **In a pod:** the **Server monitoring** data source (`monitor`, feed `overview`) serves a summary, plain-language
  checks and every counter. The data is server-wide, so a pod reads it only after
  `server pods monitoring <name> on`. Ask Claude to build an app on it.
- **Prometheus:** with `CONSTRUCT_METRICS_ADDR`, `GET /metrics` on that address serves the same counters as
  `construct_*` metrics.

## Deploying

The repository deploys a staging server to [Fly.io](https://fly.io): `deploy/pod.Dockerfile` builds the server image,
and `deploy/fly/pod.toml` configures the app (`the-construct-pod`), its volume, health check and metrics. CI deploys
it on every push to `main` once the Rust job passes, when the repository secret `FLY_API_TOKEN_POD` is set. By hand,
from the repository root:

```bash
flyctl deploy . --config deploy/fly/pod.toml --dockerfile deploy/pod.Dockerfile --remote-only
```

Set secrets with `flyctl secrets set`, never in the file: at least `CONSTRUCT_SECRETS_KEY`, and
`CONSTRUCT_INFER_API_KEY` for model features. The staging server talks to a fake Pixelfed deployed separately
(`fake-pixelfed/fly.toml`, workflow `.github/workflows/fake-pixelfed.yml`, secret `FLY_API_TOKEN_FAKE`).

Schema changes must come with a migration that works on the existing databases, since staging upgrades in place.

Any other host works the same way: run the image (or the `server` binary) with a persistent `CONSTRUCT_DATA_DIR`
behind TLS, and set `CONSTRUCT_SECRETS_KEY`.
