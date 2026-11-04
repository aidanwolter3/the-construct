# Dates and counts are formatted by the client, as two layout features on core://Text

Date: 2026-11-04. Related: D37, [collections](collections.md), [layout-features](layout-features.md), issue #55.

Collections serves dates as `YYYY-MM-DD` text, so a todo app showed "2026-11-05" instead of "Due tomorrow", and no
layout could show "3 left". Layouts can't compute (D37). The pod can't do it either: "tomorrow" depends on the
viewer's time zone and on when the screen is looked at, which the pod doesn't know and a cached screen would get
wrong the next day (decision `collections`, point 2). Considered: computed fields on the pod (rejected for that
reason), a new node per use ("DueDate", "Counter"; rejected: narrow, and they wouldn't combine with text around
them), and filters inside the binding syntax (`$item.due|relative`; rejected: the pod would have to parse every
string to work out a layout's features, and an unknown filter would show as raw text).

Decision: two additive layout features on `core://Text`, drawn by the client from its own clock:

- **`date_format`:** `format: {date: Relative|Short, past?}`. Every value the text binds (`$item.<f>`, embedded
  tokens, `${uri#f}`) that is exactly a `YYYY-MM-DD` date is drawn as `Relative` ("today", "tomorrow", "yesterday",
  "in N days" / "N days ago" up to six days, else as `Short`) or `Short` ("Nov 5", with the year when it is not this
  year). Other values are drawn unchanged, so "Due $item.due" reads "Due tomorrow". With `past`, a text binding a
  date before today draws `past` instead ("Overdue"). Today is the device's local date (`localEpochDay`), read each
  time the text draws; the simulator's `-Ptoday=YYYY-MM-DD` pins it for repeatable screenshots
  (`fixtures/custom_todos_bundle.json` is a todo app for it: `-Pworkspace=c-todos -Pfixture=… -Ptoday=2026-11-04`).
- **`count`:** `count: {query_key, where?: [{field, equals}], zero_text?}`. The number of entities of that query
  the client holds that match every condition (each as a `core://List` `where`: `equals` may be `$form.<field>`, and
  a blank one matches everything) replaces each `$count` in `value_expr`. It is counted on the device, so it follows
  optimistic ticks and deletes at once (a `deleted` condition hides a row deleted a moment ago). A count of 0 draws
  `zero_text` when set; while the query is not loaded, `$count` is empty.

The pod's validation requires a non-empty `query_key` and `where` fields, and a `$count` (or a `zero_text`); custom
layouts' query keys are checked against the app's sources as everywhere else, and Collections checks that the
`where` fields are fields of the collection. `describe_collection`'s layout hints add a `date` snippet (for the
first date field) and a `count` snippet. English only, like the rest of the vocabulary. No `SCHEMA_VERSION` bump.

Not done yet: number formatting (`{number: …}`, fractions and units for recipes); timestamps (only plain dates are
formatted; a timestamp's local day needs a parser the shared code doesn't have); styling an overdue date (it can
only replace the text, not tint it); and redrawing at midnight while a screen stays open (it updates on the next
redraw).
