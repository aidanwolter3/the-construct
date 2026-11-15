# Settings plugs in jacks, with the key typed into a hidden field

Date: 2026-11-15. Related: [jacks](jacks.md), [settings-page-and-sign-ins](settings-page-and-sign-ins.md),
[named-vocabulary](named-vocabulary.md), [layouts-not-apps](layouts-not-apps.md).

A config jack such as `web_search` (provider `brave` and its API key) could only be plugged through the MCP
`plug_jack` tool, so the key had to be pasted into a Claude conversation. Decision `jacks` left "a Jacks section in
Settings" for later and assumed it would need the list as device data (`local:`), an engine change. Two things were
missing: a way for Settings to read and write pod data, and a text field that doesn't show what is typed.

Decision:

- **The pod serves the jacks as screen data**, not device data: `GET /v1/jacks/screen` answers every jack as an
  entity `jacks://<id>` and the `jacks` query; `?id=<id>` answers one. Each entity flattens what `GET /v1/jacks`
  says into fields a layout can bind and switch on (`state_label`, `is_config`, `can_plug`, `can_unplug`,
  `has_<field>`, a config value now set, and for a secret only `<field>_set` and a hint, never its value). The Settings
  blueprint's adapter declares these paths and actions in its manifest, as any source does. The engine is unchanged.
- **Settings writes them through the outbox**: `POST /v1/jacks/plug {id, config?}` and `POST /v1/jacks/unplug {id}`,
  sent as `action.mutation.optimistic` on `jacks://<id>` (state and its flags change at once; a refusal, such as
  "brave needs api_key", rolls them back). Any session of the pod may call them, as `GET /v1/jacks`: a device that
  opened the pod with its code is the owner's. The app's plug drops blank values and names the jack doesn't take, so
  a form that sends every field it has keeps a stored secret and a stored provider when they are left blank; Claude's
  `plug_jack` is unchanged (a blank secret there still clears it). Account jacks are listed with where they are
  signed in; Sign-ins plugs and unplugs them.
- **A new field, `core://TextField` `secret`** (feature `text_field_secret`): it draws one dot per character,
  measures carets and selections on the dots (mapped back to the value's bytes), offers no Copy or Cut, tells
  Android's keyboard it is a password (no suggestions, no learning), and gives screen readers a password input whose
  value is the dots. One line, no `on_submit`: the pod refuses a secret field with `lines` or `on_submit`.
- **Settings' new section** (layout `settings` 0007): a Jacks row opens a list of the jacks with their state; a jack's
  screen shows what it does, Plug in or Unplug (asking first) for switches and config jacks, and for a config jack a
  form: the provider as chips, the key in a secret field, the SearXNG address. The form knows today's config fields
  (`provider`, `api_key`, `base_url`) and shows each only when the jack has it: a config jack with a new field needs
  a new Settings version.

Builds without `text_field_secret` keep getting Settings 0006 (no Jacks section), so seeing it needs a new desktop
or Android build.

Not yet: after a plug the list shows "Plugged in" until its data is loaded again (reopening the screen shows the
provider); Settings isn't on home's data stream, so another device's change shows on the next open. Image search
(Brave's image endpoint) isn't part of the `web_search` jack.
