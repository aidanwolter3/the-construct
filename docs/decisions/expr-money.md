# Prices in computed fields: `money(x)`, and whole numbers without `.0`

Date: 2026-11-20. Related: [collection-computed-fields](collection-computed-fields.md),
[research-per-row](research-per-row.md).

A UX audit of the developer's pod found shopping prices drawn as `$98.0`, `$45.0` and `$28.5`. They come from an
`expr` field, `"$" + entity.price`: a price stored as the float `98.0` was joined to text as `98.0`, and nothing could
write two decimals.

Decision: **a number joined to text is written as people write it (`98`, never `98.0`), and the expression language
has one function, `money(x)`, that writes a number as a price.** `money` rounds to cents, groups thousands with
commas, leaves whole amounts without cents and writes the rest with two (`$98`, `$28.50`, `$1,299.95`, `-$4.20`); an
empty value gives empty text, so `entity.now ? money(entity.now) : entity.note` falls back cleanly; text that isn't a
number is an error, as for `*`.

Why a function in a language that had none: a price is the one formatting need the pods show, and the alternatives
were worse. A model field costs a call per price change for a deterministic answer; a client `format` on
`core://Text` needs a new app build and leaves every other reader of the field (notifications, agents, exports)
with the raw number. `money` is parsed like a parenthesis after a known word, so the language stays small: no other
names, no user functions, the same length and depth limits.

Not done: other currencies and locales (`$` only, as the pods' sources are US stores); a client-side number format.

Consequences: server only, no client build. Existing `"$" + entity.x` fields read `$98` instead of `$98.0` after the
deploy, without a change; a pod switches a field to `money(entity.x)` with `apply_change` `update_field`.
