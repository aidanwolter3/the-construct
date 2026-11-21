# Running the server

`cargo run -p server` starts one server that hosts many pods. This page covers configuration, pods, monitoring and
deployment. The HTTP API is in [api.md](api.md).

## Locally

```bash
cargo run -p server                       # http://127.0.0.1:8080
cargo run -p fake-pixelfed                # optional: a fake Pixelfed upstream on 127.0.0.1:8081
curl -s http://127.0.0.1:8080/v1/construct | jq .        # home's root element
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
| `CONSTRUCT_DATA_DIR` | `~/.construct-server` | `host.db` (pods, token hashes, the shelf of shared publications) and `pods/<id>/server.db` (each pod's data). Created with mode 0700, databases 0600. |
| `CONSTRUCT_PUBLIC_URL` | from each request's `Host` | The base URL clients reach the server at, for the OAuth metadata Claude reads and the redirect URI of a pod's MCP server sign-ins. `https` is assumed behind Fly or with `X-Forwarded-Proto: https`. |
| `CONSTRUCT_OAUTH_CLIENT_ID` | unset | A pre-registered OAuth client for MCP apps that ask for a client ID and secret instead of registering (e.g. Gemini), decision [`oauth-preregistered-client`](decisions/oauth-preregistered-client.md). Any value you choose; set with `_SECRET`. |
| `CONSTRUCT_OAUTH_CLIENT_SECRET` | unset | That client's secret (a Fly secret, e.g. `openssl rand -hex 32`). The connect page still asks for the pod code. |
| `CONSTRUCT_OAUTH_CLIENT_REDIRECT_URIS` | empty | That app's OAuth callback URL(s), comma-separated. An unknown one is shown on the connect page so you can add it. |
| `CONSTRUCT_SECRETS_KEY` | unset | Seals upstream credentials (Pixelfed tokens, OAuth client secrets) with AES-256-GCM: base64 of 32 random bytes (`openssl rand -base64 32`). Comma-separate keys to rotate: the first seals, all open. Unset: plaintext with a warning (an error on Fly). A sealed row whose key is missing reads as signed out. Production must set it (decision [`upstream-credentials-sealed-at-rest`](decisions/upstream-credentials-sealed-at-rest.md)). |
| `CONSTRUCT_SESSION_TTL_DAYS` | `90` | Lifetime of sessions other than pod tokens (1..=36500). Pod tokens don't expire. |
| `RUST_LOG` | `info` | `tracing` filter, e.g. `server=debug,info`. |

### Pods

| Variable | Default | Meaning |
|---|---|---|
| `CONSTRUCT_OPEN_POD` | `dev` on loopback, else unset | Callers without a pod token use this pod. Development only; `off` disables it. Refused on a non-loopback address (unless the next variable is set) and always on Fly. Only requests whose `Host` is `localhost`, `*.localhost` or an IP address use it, so a DNS-rebinding page can't. |
| `CONSTRUCT_WEB` | unset | `1`: serve the web client at `/web` (decision [`web-client`](decisions/web-client.md), [clients.md](clients.md#web)), take its session cookie, and proxy images for it. Unset, none of these exist. |
| `CONSTRUCT_WEB_DIR` | `target/web` | Where `scripts/web_build.sh` put the web client. |
| `CONSTRUCT_PUBLIC_WEB` | unset | `1` (with `CONSTRUCT_WEB=1`): admin pods (not temporary ones) may share public links, a layout's copy anyone can open at `/s/<link>` or a chosen `/<handle>/<slug>` (decisions [`public-web-links`](decisions/public-web-links.md), [`public-links-share-the-closure`](decisions/public-links-share-the-closure.md)). Unset, the tools aren't listed and `/s/…` and the short paths answer 503 "Sharing is paused on this server"; the stored copies stay, and setting it again brings every link back. The links `share_publicly` answers start with `CONSTRUCT_PUBLIC_URL` when it is set. The same setting gates calendar feeds (decision [`ical-feeds`](decisions/ical-feeds.md)): admin pods may publish a private `.ics` of chosen rows at `/ical/<token>.ics` for a calendar app (Proton Calendar subscribes from its own servers, so the server must be reachable publicly, as staging is); unset, `/ical/…` answers 503 and the stored feeds stay. |
| `CONSTRUCT_INSECURE_OPEN_POD_ON_NETWORK` | unset | `1` allows the open pod on a non-loopback address: everyone who can reach the server gets that pod and its sign-ins. Only on a network you trust. |
| `CONSTRUCT_TEMP_PODS` | unset | `1`: anyone may create a temporary pod (`POST /v1/pods/temporary`). |
| `CONSTRUCT_TEMP_POD_HOURS` | `24` | Temporary pod lifetime (1..=720). |
| `CONSTRUCT_TEMP_POD_MAX` | `50` | Live temporary pods at most. |

### Storage limits (decision [`pod-storage-limits`](decisions/pod-storage-limits.md))

| Variable | Default | Meaning |
|---|---|---|
| `CONSTRUCT_UPLOAD_MAX_BYTES` | 20 MB | Largest single upload (`POST /v1/uploads`). |
| `CONSTRUCT_POD_UPLOAD_QUOTA_BYTES` | 500 MiB | All of an admin pod's uploads together. |
| `CONSTRUCT_TEMP_POD_UPLOAD_QUOTA_BYTES` | 6 MiB | The same for a temporary pod. |
| `CONSTRUCT_POD_STORAGE_QUOTA_BYTES` | 768 MiB | An admin pod's whole database: uploads, layouts and their versions, components, collections. Past it, writes get 507 `pod_storage_full`. |
| `CONSTRUCT_TEMP_POD_STORAGE_QUOTA_BYTES` | 8 MiB | The same for a temporary pod. |
| `CONSTRUCT_TEMP_POD_TOTAL_BYTES` | 400 MiB | All temporary pods together: a new one is refused (503 `temporary_pods_full`) when the live ones, each counted at its storage quota, would pass it. With the defaults, 50 fit. |
| `CONSTRUCT_MIN_FREE_DISK_BYTES` | 100 MiB | Below this free space, uploads, new temporary pods and OAuth client registrations are refused, so other writes keep working. |

### Event streams

Open event streams (`GET /v1/construct/events` and any other `text/event-stream` answer) are capped by the host, on
top of each pod's 8 per session token. Past a cap the stream is refused: 429 `too_many_streams` for the caller's
address or pod, 503 `too_many_streams` when the server (or its temporary pods together) is full.

| Variable | Default | Meaning |
|---|---|---|
| `CONSTRUCT_SSE_MAX_PER_IP` | `24` | Per client address (an IPv4 address, an IPv6 /64). |
| `CONSTRUCT_SSE_MAX_PER_POD` | `32` | Per pod. |
| `CONSTRUCT_SSE_MAX_TEMPORARY` | `80` | All temporary pods together, so visitors can't take every stream from admin pods. |
| `CONSTRUCT_SSE_MAX_TOTAL` | `200` | All pods together (Fly's `hard_limit` is 250 connections). |

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
| `CONSTRUCT_INFER_API_KEY`, `CONSTRUCT_INFER_MODEL` | unset | The agent module's key (every model call goes through `crates/agent`) and its cheap model: both set, `model`-computed collection fields are answered by it. A pod with its `model` jack unplugged or locked (temporary pods) never uses it. |
| `CONSTRUCT_INFER_URL` | `https://api.anthropic.com/v1/messages` | The Messages API endpoint. |
| `CONSTRUCT_FILL_MODEL` | `claude-opus-5-5` | The agent module's capable model: `agent`, `extract: llm` and `query_field` fillers (same key and endpoint). Without a key those fillers report themselves unavailable. Each pod's model requests (retries included) have daily budgets per feature and in all (3,000), counted in `agent_usage`. |
| `CONSTRUCT_INFER_POD_TOKENS_PER_DAY` | `2000000` | Each pod's daily model token budget (uncached input, cache writes and output; cache reads aren't counted), in `agent_usage`. Over it, that pod's model features refuse until the next UTC day. |
| `CONSTRUCT_INFER_MAX_TOKENS_PER_DAY` | `5000000` | The server's daily model token cap across all its pods (they share the one key), counted the same way. Over it, every model jack answers "server budget reached" until the next UTC day. Counted in memory: a restart counts the day afresh. |
| `CONSTRUCT_FILL_AGENT_DAILY` | `20` | Agent filler runs per pod per day. |
| `CONSTRUCT_FILL_PRIVATE_HOSTS` | unset | `1`: fetch fillers may read private addresses, and MCP servers may be on them (local development only). |
| `CONSTRUCT_BROWSER` | the first `chromium`, `chromium-browser`, `headless_shell`, `chrome-headless-shell` or `google-chrome` on `PATH` | The headless browser of rendered fillers (`render`, decision `rendered-fetches`); `off`: none, and rendered fillers say so. The image sets `/usr/bin/chromium`. It starts with none of the server's environment, and as `nobody` (uid 65534) when the server is root, so a page that takes over a renderer can read neither the keys nor the pods' data. It runs with Chromium's sandbox and site isolation where they work: the first render after the server starts tries them and logs `browser sandbox: on`, or falls back to `--no-sandbox` and logs a `browser sandbox: off` warning with the reason (decision `browser-sandbox`). At startup a root server also fences that user's network with an nftables rule, so the only connection it may open is to the browser's proxy on `127.0.0.1:1009` (not the server's port, its metrics, Fly's private network or any private address), and logs `browser private network block: on`, or a `browser private network block: off` warning with the reason (no `nft`, no netfilter) and serves anyway; on Fly, a server with neither the rule nor the sandbox then renders no pages. It also closes Fly's `/.fly/api` socket to other users if it was open (decision `browser-network-fence`). |

### Monitoring and development

| Variable | Default | Meaning |
|---|---|---|
| `CONSTRUCT_METRICS_ADDR` | unset | Serve Prometheus `GET /metrics` on this separate address. |
| `CONSTRUCT_MONITOR_AUTH_FAILURES_PER_HOUR` | `20` | The monitoring source warns above this many wrong pod codes and unknown tokens an hour. |
| `CONSTRUCT_MONITOR_LLM_TOKENS_PER_DAY` | `1000000` | It warns above this many model tokens a day: uncached input, cache writes and output. Tokens read from the prompt cache (a twentieth of the price) are counted (`direction="cache_read"`) and shown, but not toward this. |
| `CONSTRUCT_MONITOR_TEMP_PODS_PERCENT` | `80` | It warns once live temporary pods reach this share of `CONSTRUCT_TEMP_POD_MAX` (40 of 50), before new ones are refused. |
| `CONSTRUCT_AGENTS_PRIVATE_HOSTS` | unset | `1`: the Agents source may sign in to a runner on any address over http (local development only); otherwise `https://` public hosts. |
| `CONSTRUCT_SHOW_DEV_APPS` | unset | `1`: MCP also lists the dev core layouts and sources (`pixelfed_sample_no_login`; it works either way). |
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
server pods gaps [name]                        # what agents said was missing while building (MCP note_gap)
server pods adopt <name> <path/to/server.db>   # turn a pre-pods single-user database into a pod
server pods remove-adopted [dir]               # delete the originals adopt left behind
server backup <dir>                            # a consistent copy of host.db and every pod's database
```

`list` shows, for each pod, whether it is on the content-addressed construct (`construct yes`), and for a temporary
pod who asked for it (`created by ip …, client "…", agent "…"`). A deleted pod stops at once (its tokens end and it no
longer counts toward `CONSTRUCT_TEMP_POD_MAX`); `list` shows it as `deleted` until the sweeper removes its files and
row, within 5 minutes. Every pod is on it
(decision [`one-layout-system`](decisions/one-layout-system.md)): a pod still on the old layout system is converted
when it next opens. The conversion draws each of its layouts both ways on empty data first and logs the result: one
`info` line when every layout draws the same, a `warn` line per layout that differs (with its differences)
otherwise. The pod is switched either way, and the old tables are then dropped. If the conversion itself fails, the
pod doesn't open (the host answers `503 pod_unavailable`), its old tables are kept, and the next open tries again;
the log says why.

Pod codes never go into git or config files; keep yours in a password manager.

### Fixed codes for local testing

To test on your own machine with codes that don't change between runs, give `serve` the codes yourself:

```bash
cargo run -p server -- serve --pod-code cpod_localtest123 --mcp-key cmcp_localtest123
```

The server makes an admin pod named `local` (another with `--pod-name <name>`) if there is none, makes it open with
`--pod-code`, and adds `--mcp-key` as one of its MCP credentials. Run it the same way every time and the desktop app
and `claude mcp add --transport http construct-local http://127.0.0.1:8080/v1/mcp --header "Authorization: Bearer
cmcp_localtest123"` keep working. A different `--pod-code` replaces the pod's code (ending its sign-ins, as `rotate`
does). The flags are refused on Fly and on a non-loopback `CONSTRUCT_ADDR`: codes on a command line are for this
machine only.

### Temporary pods

With `CONSTRUCT_TEMP_PODS=1`, `POST /v1/pods/temporary` makes a pod that is deleted with everything in it after
`CONSTRUCT_TEMP_POD_HOURS`. Temporary pods have every source, but Pixelfed only on the default instance (no typed-in
servers), the `model`, `web`, `web_search`, `mcp` and `agent_runner` jacks locked (no model calls, no fetch fillers,
no searches, no MCP servers, no agent runner sign-in: 403 `jack_locked`; decision `jacks`), no handle and no
publishing (`set_handle` and `publish` answer `temporary_pod`), no public links, and smaller storage and upload
quotas, with a cap on all of them together (`CONSTRUCT_TEMP_POD_TOTAL_BYTES`) and on their event streams together
(`CONSTRUCT_SSE_MAX_TEMPORARY`). At most 5 are made per client address an hour. They are how visitors and automated sessions try a deployed server:

```bash
pod=https://the-construct-pod.fly.dev
token=$(curl -fsS -X POST -H "X-Construct-Client: my-script" $pod/v1/pods/temporary | jq -r .session_token)
curl -fsS -H "Authorization: Bearer $token" $pod/v1/construct | jq      # {root, generation}
curl -fsS -X DELETE -H "Authorization: Bearer $token" $pod/v1/pods/temporary   # done: free the slot now
```

The server records who asked for each one: the client address (`Fly-Client-IP` behind Fly), the caller's own
`X-Construct-Client` label and its `User-Agent`, in the host database and in the `temporary pod created` log line
(refusals are logged the same way). `server pods list` shows it. The label is whatever the caller sends, so it is a
hint next to the address, not proof: CI's smoke test sends `ci-smoke-test run <id>` and sessions `claude-session
<id>`; a pod with no label, or a label from an address you don't know, is someone else's. The apps don't send a
label yet. A temporary pod deletes itself with `DELETE /v1/pods/temporary` and its own token, as the deploy smoke test
does when it finishes.

## Security defaults

- Every response carries `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `X-Frame-Options: DENY`
  and `Content-Security-Policy: default-src 'none'; frame-ancestors 'none'` unless the route sets its own; behind Fly
  also `Strict-Transport-Security`.
- Opening a pod by code is limited to 10 tries a minute per client address (behind Fly, `Fly-Client-IP`).
- Outbound requests to user-chosen hosts (typed Pixelfed servers, push endpoints, MCP servers, agent runners, filler
  URLs, the image proxies `/web/image` and `/v1/image`) all go through `pod_core::net::OutboundClient` (decision
  [`outbound-client`](decisions/outbound-client.md)): the URL is checked first (scheme, a public host name, no IP
  literal, the port), names resolve only to public addresses, redirects never lead into private space, and a body is
  read only up to a cap (4 MiB unless the caller sets another; Pixelfed API answers 4 MiB). `clippy.toml` refuses
  any other `reqwest` client outside `net.rs`.

## Monitoring

The server counts what an operator wants to notice: wrong pod codes and unknown tokens, rate limits hit, credentials
used out of scope, new Claude connections, 5xx responses, temporary pod requests, and model calls and tokens per
feature (by direction: `input`, `cache_write`, `cache_read`, `output`), plus pods, credentials and free disk (decision [`server-monitoring`](decisions/server-monitoring.md)).
Counts live in memory and restart from zero.

- **In a pod:** the **Server monitoring** data source (`monitor`, feed `overview`) serves a summary, plain-language
  checks and every counter. The data is server-wide, so a pod reads it only after
  `server pods monitoring <name> on`. The built-in layout **Server health** (core layout `server_health`,
  `crates/source-monitor/layouts/server_health/`) shows it: the status, the checks, the devices (below) and every
  counter. Use it whole, `{"type": "core://Use", "ref": "@core/server_health"}` as a layout's root with feeds
  `[["monitor","overview"],["monitor","devices"]]`, or open it by reference (`action.navigation.open` with
  `"ref": "@core/server_health"`); no agent has to write the screen. In a pod without monitoring it says that
  monitoring is off for this pod and shows the devices alone (it reads `${core://sources/monitor#error}`, `error 403`
  there).
- **Alerts:** in such a pod, the layout that shows the source gets a "Server warnings" notification channel
  (decision [`notification-owner`](decisions/notification-owner.md)). The
  adapter looks at the checks every 5 minutes (while the pod has a registered device), and a check that turns to
  warning sends one notification ("Server: <check>"); it notifies again only after it has been back to ok (decision
  [`server-monitoring-alerts`](decisions/server-monitoring-alerts.md)).
- **Devices** (decision [`device-versions`](decisions/device-versions.md)): every pod records the devices that reach
  it (one per app install or browser, from `x-construct-device`), the build each runs (`x-construct-build`) and the
  build each ran each day. Only a request with one of the pod's sessions counts: one on a public path (an upload, a
  Pixelfed image, the OAuth callback), which anyone with the pod's id can reach, is no device of the pod. The `monitor` source's `devices` feed shows them in any pod (no operator switch): how
  each device's build stands against the server's (latest, behind, ahead) and devices per day by build for a
  `core://Bars` chart. Over MCP, `list_devices`, `rename_device` and `watch_device` (run a source action or send a
  "Device updates" notification once a device draws a feature or runs a newer build).
- **Prometheus:** with `CONSTRUCT_METRICS_ADDR`, `GET /metrics` on that address serves the same counters as
  `construct_*` metrics.

## Backups and restore

Everything a server keeps is in `CONSTRUCT_DATA_DIR`: `host.db` (pods, credentials, the shelf, public links) and
`pods/<id>/server.db` (each pod's data, uploads included). Public links are shared with people who expect them to
keep working (decision [`public-links-share-the-closure`](decisions/public-links-share-the-closure.md)), so staging
keeps two kinds of backup:

- **Volume snapshots** (Fly): the volume `pod_data` is snapshotted daily and each snapshot kept 5 days
  (`scheduled_snapshots`, `snapshot_retention` in `deploy/fly/pod.toml`, Fly's defaults). They cover losing the
  machine or the volume. To restore one, make a volume from it and a machine on that volume:

  ```bash
  fly volumes list -a the-construct-pod                         # the volume's id (vol_…)
  fly volumes snapshots list <vol id>                           # pick a snapshot (vs_…)
  fly volumes create pod_data -a the-construct-pod --snapshot-id <vs id> --region dfw --size 1
  fly machine list -a the-construct-pod                         # the running machine's id
  fly machine stop <machine id> -a the-construct-pod
  fly machine clone <machine id> -a the-construct-pod --attach-volume <new vol id>:/data
  ```

  then, once the new machine's server looks right, destroy the old machine and its volume
  (`fly machine destroy <machine id>`, `fly volumes destroy <old vol id>`); later deploys update the new one.
- **`server backup <dir>`**: a consistent copy (SQLite `VACUUM INTO`, safe while the server runs and writes) of
  `host.db` and every pod's `server.db`, into a new or empty folder. Make one before a risky migration or deploy,
  and keep a copy off the machine:

  ```bash
  fly ssh console -a the-construct-pod -C "/server backup /data/backups/$(date -u +%F)"
  fly ssh console -a the-construct-pod -C "tar -C /data/backups -czf /tmp/backup.tgz $(date -u +%F)"
  fly ssh sftp get -a the-construct-pod /tmp/backup.tgz ./construct-backup.tgz
  ```

  The backup takes room on the same 1 GB volume: delete old ones (`rm -r /data/backups/<date>`).
  **To restore it**, put its folder at `<data dir>/restore` (on Fly: `mv /data/backups/<date> /data/restore`, or
  upload one with `fly ssh sftp put` and unpack it there) and restart the server (`fly machine restart`). On start,
  before any database opens, the server moves the current `host.db` and `pods/` to `<data dir>/replaced-<unix time>`
  and the backup into their place, and logs `restored the data from a backup`. Delete the replaced folder once the
  restored server looks right.

Nothing copies the data off Fly automatically yet: that needs object storage (a new paid service and a secret).

## Deploying

The repository deploys a staging server to [Fly.io](https://fly.io): `deploy/pod.Dockerfile` builds the server image,
and `deploy/fly/pod.toml` configures the app (`the-construct-pod`), its volume, health check and metrics. The image
is Debian slim with Chromium for rendered fillers (decision `rendered-fetches`), on a 1 GB machine. CI deploys
it on every push to `main` once the Rust job passes, and every Monday, when the repository secret
`FLY_API_TOKEN_POD` is set. Each deploy passes the ISO week as `CHROMIUM_REFRESH`, so the image's Chromium and
Debian packages are reinstalled at least weekly rather than kept in a cached layer (decision `browser-sandbox`). The
web client's build waits for the server's: run at once on Fly's remote builder, the two release builds ran it out of
memory. By hand, from the repository root:

```bash
flyctl deploy . --config deploy/fly/pod.toml --dockerfile deploy/pod.Dockerfile --remote-only \
  --build-arg CHROMIUM_REFRESH="$(date -u +%G-W%V)" --build-arg CONSTRUCT_BUILD="$(scripts/build_id.sh)"
```

`CONSTRUCT_BUILD` is the build id the server and its web client report (decision `device-versions`; the image has
no `.git` to make it from). Without it they report `<version>+dev`, which can't be ordered against the apps'.

Whether staging's browser runs sandboxed shows in its log after the first render since the deploy (a fetch filler
or `collections_preview` with `render`): `fly logs -a the-construct-pod | grep "browser sandbox"`. Whether its user
is fenced shows at every start: `grep "browser private network"` (decision `browser-network-fence`). To check the
fence itself, `fly ssh console -a the-construct-pod`, then `nft list table inet construct_browser` shows the rule,
and as the browser's user the server's port and the metrics port are refused, while root connects:

```bash
# The image has no curl; bash's /dev/tcp opens the connection.
setpriv --reuid=65534 --regid=65534 --clear-groups bash -c 'echo > /dev/tcp/127.0.0.1/8080'      # refused
setpriv --reuid=65534 --regid=65534 --clear-groups bash -c 'echo > /dev/tcp/127.0.0.1/9091'       # refused
bash -c 'echo > /dev/tcp/127.0.0.1/8080 && echo > /dev/tcp/127.0.0.1/9091'                       # connects
```

Set secrets with `flyctl secrets set`, never in the file: at least `CONSTRUCT_SECRETS_KEY`, and
`CONSTRUCT_INFER_API_KEY` for model features. The staging server talks to a fake Pixelfed deployed separately
(`fake-pixelfed/fly.toml`, workflow `.github/workflows/fake-pixelfed.yml`, secret `FLY_API_TOKEN_FAKE`).

Schema changes must come with a migration that works on the existing databases, since staging upgrades in place.
A migration is a named step appended to its list (the core's in `pod-core/src/store.rs`, a source's in its
`migrations()`, `host.db`'s in `pod-host/src/db.rs`); `schema_migrations` records which names ran (decision
`named-migrations`). Never rename, edit or remove a step that shipped. If another branch also appended a step, git
may report a conflict at the end of the list: keep both.

Any other host works the same way: run the image (or the `server` binary) with a persistent `CONSTRUCT_DATA_DIR`
behind TLS, and set `CONSTRUCT_SECRETS_KEY`.
