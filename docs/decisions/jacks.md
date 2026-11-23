# Pods have jacks: the abilities each pod is plugged into

Date: 2026-11-09. Related: D46, [collection-fillers](collection-fillers.md),
[agents-data-source](agents-data-source.md), [settings-page-and-sign-ins](settings-page-and-sign-ins.md),
[web-and-agent-modules](web-and-agent-modules.md).

What a pod could do beyond layouts and data was spread over unrelated switches: `collections.model = 0` and
`collections.fill = 0` in `pod_settings` (set on temporary pods), the agent runner's sign-in in its own table, the
Pixelfed sign-in on sessions, a server-wide model key, and collection secrets for fillers. Nothing listed them, so
neither Claude nor a person could see what a pod was plugged into, and a new ability (web search with the user's
own key) had nowhere to live.

Considered: one more pod setting per ability (no shape for config or secrets, nothing lists them); collection
secrets for the search key (they belong to fillers' headers and are bound to one host, not to an ability);
keeping account sign-ins apart (then "what can this pod do" still has no single answer).

Decision: a **jack** is one ability a pod can be plugged into (`pod_core::jacks`). Every pod has the same set,
declared by the core (`model`, `web`, `web_search`) and by adapters (`Source::jacks`: `pixelfed`,
`agent_runner`); ids are unique (checked at startup) and stored, so never renamed. Three kinds:

- **switch** (`model`, `web`): on or off, using the server's own resources. Default on.
- **config** (`web_search`): plugged with values, some secret (sealed with `CONSTRUCT_SECRETS_KEY`, context
  `jacks:<id>:<field>`, never shown back; a re-plug that leaves a secret out keeps it). The search key is the
  pod's own: each user brings theirs.
- **account** (`pixelfed`, `agent_runner`): the adapter's own sign-in flow plugs it; the adapter reports its state
  (`jack_status`, stored data only) and may unplug it (`unplug_jack`: the runner signs out; Pixelfed is signed
  out from Settings).

State is the `jacks` table (no row: the default). The host **locks** a jack off for a pod with `jack.<id> =
locked` in `pod_settings`: temporary pods get `model`, `web`, `web_search`, `mcp`, `agent_runner` and `taste` locked
(`agent_runner` since issue #445: its sign-in made the server follow any public host, and its sign-in route refuses
with 403 `jack_locked`). The host writes these locks when it makes a temporary pod and again each time it opens one,
so a jack locked by a newer server is locked on the temporary pods an older one made (security audit 2026-11-23:
`taste` reached only pods made after it shipped). They replace
`collections.model` and `collections.fill` (a Collections migration turns those old settings into locks). A
locked jack reads as unplugged and can't be plugged. Features check their jack: the agent module refuses model
calls with `model` unplugged, fillers record "the pod's web jack is unplugged", `query_field` fillers name
`web_search`.

Claude sees and changes jacks with `list_jacks`, `plug_jack` and `unplug_jack`; `GET /v1/jacks` lists them for a
signed-in caller. No client build: Settings is unchanged.

Not yet: a Jacks section in Settings (needs the list as device data); per-pod model keys (the server's one key,
`CONSTRUCT_INFER_API_KEY`, stays); moving collection secrets onto jacks.
