# Text weight, capitals and line limits; inverted and query conditionals

Date: 2026-11-17. Related: [text-dates-and-counts](text-dates-and-counts.md),
[date-conditions](date-conditions.md), [named-vocabulary](named-vocabulary.md),
[dashed-box-borders](dashed-box-borders.md).

A pod redesign needs typography the seven `core://Text` styles can't give: a medium or semibold line inside a
card, small spaced-out capitals above a title ("MOVE OFTEN · THIS WEEK"), and a note cut to two lines. It also
needs conditionals the vocabulary lacked: an "else" branch for any test, and a card that hides while the list in
it is empty. Each of these could be faked per pod (a second text copy in capitals, a computed "has rows" field),
but they are generic, so they belong in the plumbing.

Considered for the weights: synthesising bold (cosmic-text 0.19 can't embolden; doing it in both rasterizers would
draw differently from the real cuts) or letting fontdb pick the nearest bundled weight (Medium would draw as
Regular and SemiBold as Bold, so the field would do nothing visible). Considered for the line limit: cosmic-text's
own `Ellipsize`, which counts lines per paragraph, so `"One\nTwo\nThree"` with two lines would draw three.

Decision:

- `core://Text` gets `weight: Regular|Medium|SemiBold|Bold`, `caps: bool` and `max_lines: 1..20` (features
  `text_weight`, `text_caps`, `text_max_lines`). Each weight is a real font file: Instrument Sans and Lora at 500,
  600 (and Lora at 700) are static cuts of the families' variable fonts (google/fonts, OFL, the same version as the
  bundled regular and bold), made with fontTools' `varLib.instancer`; they add about 470 KB to the clients. `caps`
  upper-cases the drawn text and spaces it by 0.07 em; the semantic text stays as written, so screen readers
  don't spell it out. `max_lines` is applied where text is shaped, so measuring, the CPU and the GPU renderer
  agree: a longer text is cut to the longest start that fits with "…" (bisection over its characters).
- The three text features don't gate a layout (as `box_border_style`): they are styling, so a client without them
  is still served the layout, ignores the fields (serde defaults) and draws the text in its style.
- `core://Conditional` gets `not: bool` (feature `conditional_not`), which inverts the test (truthy, `equals`,
  `date`, or `count`; a value that isn't a date fails the date test, so `not` shows it), and `count: {query_key,
  where?}` (feature `conditional_count`, a `core://Text` `count` without `zero_text`), which shows the content
  while the query has at least one entity matching every `where`. `value` becomes optional (still written, as "",
  because older clients require it). With `count` the query is the whole test: `value`, `equals` and `date` are
  refused. While the query isn't loaded, nothing shows, with or without `not`, so a screen doesn't flash its
  "empty" branch before the data comes.
- Both conditional features gate the layout: a client that ignored them would show content when it shouldn't.

Not done: italics, other weights (Light, Black) and letter spacing as a field of its own. A later weight is a new
variant with its own feature, since a client that doesn't know a variant can't decode the text.
