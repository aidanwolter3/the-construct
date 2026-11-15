# Model-computed fields are asked in batches, failed answers are retried, and a learning lookup can make its rules

Date: 2026-11-15. Related: [collection-computed-fields](collection-computed-fields.md) (amended here),
[collection-lookups](collection-lookups.md) (amended here), [live-computed-fields](live-computed-fields.md),
[jacks](jacks.md), issue #230.

A `model` field asked the model once per distinct input, one call after another. Adding one to a collection of 273
ingredients took about 20 minutes to fill. An answer that failed (an error, or text that doesn't fit the field) was
logged and dropped, so the row kept its default for good: a layout that shows a spinner while the value is the default
spun forever, and only `recompute_field` by hand got it out. Keyword rules in front of the model (a learning lookup)
saved most calls, but only where someone had first made the rules collection by hand.

Considered:
- Running single calls concurrently. Rejected: as many requests, against the same daily budget, and still a prompt
  per input.
- A per-row "retry" flag in the row's values. Rejected: values are the user's data, served to clients; what failed
  is the pod's own state.
- A `rules` option on `model` (rejected before, in `collection-lookups`): the lookup is already the primitive;
  what was missing is only that its collection had to exist.

Decision:

## Batches

The inputs waiting for one field are asked about together: up to 50 in one call (10 for a text field, whose answers
are longer). The prompt gives the field's question once, then the items numbered from 1, each as a JSON object of its
source fields; the answer is structured output, `{"answers": [{item, answer}]}` (a plain map from number to answer, or
JSON in a code fence, is read too). Each answer is coerced to the field's type as a single answer is. An item the
batch leaves out, answers twice, or answers with something that doesn't fit is asked about alone, as before. Answers
are still cached per input (or learned as rules), and one batch's answers are stored, and reach devices, in one
transaction and one data event. `FieldModel::answer_batch` defaults to "can't", so a model that answers one at a time
still works.

## Retries

An input whose answer still fails is recorded in a new table, `collections_compute_failures` (collection, field,
input key, the rows waiting on it, attempts, next try), not in the rows. The adapter's hourly task (which also runs
when the pod opens) asks again about the ones that are due: after 30 minutes, then doubling, at most 5 tries in all.
Before asking, each waiting row is computed again in the write: one set by hand, deleted, or with other inputs is
skipped; one a rule or the cache answers now is set without the model; an input no row waits on any more is forgotten.
A success clears the record. `describe_collection` shows, per computed field, `failed: {retrying, gave_up}` (rows), and
a `fix` when some gave up. `recompute_field`, or a change to the field's compute, starts its attempts afresh.

## Rules collections made on demand

A lookup with `learn: true` may name a collection that doesn't exist yet. `define_collection` and `apply_change`
(`add_field`, `update_field`) then make it in the same transaction: its `key` a text field and its `value` a field of
the computed field's type (a choice with the same options). The answer says which it made (`created_rules`). A
collection that exists is used as it is, under the checks of `collection-lookups`. A ref field's rules collection isn't
made (define it first). It counts against the pod's collection cap, and a deleted collection of that name is refused
(restore it or name another).

Consequences:
- One migration (`compute_failures`, the new table); no protocol change and no client build.
- A model call answers many rows, so a model field over a few hundred rows fills in a handful of calls: about a minute
  instead of twenty.
- A row whose input gave up keeps its default until `recompute_field` or a hand edit; describe_collection says how
  many.
- Not done: batching across fields (each field's prompt differs), or across collections.
