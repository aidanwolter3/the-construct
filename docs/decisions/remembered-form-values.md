# Layouts mark form values the device remembers; the engine stores them per app

Date: 2026-11-03. Related: D34, D44, D48.

The Pixelfed List/Cards switch (D48) is a form value, so it reset to List on every start ("Not yet" in D48). The
user wanted such choices remembered per device (each surface on its own) without the clients knowing any app.

Considered: remembering every `core://ChoiceChips` automatically (no layout change, but filters such as the recipe
categories would stick too, and typed text and photos must never be kept); a server-side setting per user (needs an
endpoint per app and syncs across devices, which was not asked for); platform preferences (Android
`SharedPreferences`, Java `Preferences`: two implementations, and the simulator would need a third).

Decision: the layout opts in. `core://ChoiceChips` takes `"remember": true` (additive schema 10). The engine keeps
remembered values in its `cache.db` (store v7, `form_values`: app id, scope, field, value), keyed by its own app id;
the scope is `""` for the app's layout (its slots) and the screen id for a screen's form. The shared UI:

- fills the root form from the device inside the snapshot callback, before the layout first draws, once per field
  per app (a pinned version and "follow latest" do the same); a screen's form when the screen opens;
- restores a value only if it is still one of the chips' options (a later layout that drops an option shows the
  default view, not an empty one); an empty value is the default option and is stored as "nothing";
- saves on every tap of a remembered chip (`FormMemory`); a local SQLite write.

Values are device preferences, not private data: never sent to the pod, kept at sign-out and "Leave pod". Engine
caps: scope and field at most 128 bytes, value at most 1 KB. A screen's remembered value is shared by all its
routes (e.g. every post), by screen id.

Not yet: `remember` on other inputs (`core://TextField`, e.g. a last search); syncing choices across devices;
forgetting values of fields no layout uses any more (tiny rows, kept).
