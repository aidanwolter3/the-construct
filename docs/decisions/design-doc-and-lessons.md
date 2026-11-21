# One design document, read by a pod as a course

Date: 2026-11-21. Related: [collection-fillers](collection-fillers.md), [public-web-links](public-web-links.md),
[public-links-share-the-closure](public-links-share-the-closure.md), [text-markdown](text-markdown.md).

The architecture was explained in pieces: `docs/architecture.md` listed the parts, the guides went deep on each,
and nothing walked a newcomer from the goals down through the pieces and their interfaces. The developer also wanted
that explanation as a course on their pod, like the Design School course there (tracks, lessons, cards, a glossary),
shared publicly and linked from the README, without two copies drifting apart.

Considered:

- **Two hand-kept copies** (a Markdown file and rows on the pod). Simple, but every change to the design would need
  both edited, and one would be forgotten.
- **The pod as the source** and the file exported from it. The design would then live outside review: a pull request
  that changes the system couldn't change its explanation in the same diff.
- **The file as the source, read into data the pod fetches** (chosen).

Decision:

- `docs/design.md` is the design, written breadth first: goals, the map of the big pieces and their interfaces, then
  one part per piece (the pod and host, layouts and the construct, sources, jacks and modules, the device side,
  building with an agent, working on the code) and a glossary. It is a guide like the others: a change to the system
  that it describes updates it in the same pull request.
- Its shape is a course: `## Part <n> · <title>` a track, `### <n>.<m> <title>` a lesson, `#### <heading>` a card;
  the first paragraph is the summary; `> **Why it matters:**` and `> **Try it:**` quotes are the lesson's callouts;
  `## Glossary` items are terms. Other `##` sections are for readers of the file.
- `crates/design-lessons` reads it into `docs/design-lessons.json` (tracks, lessons, cards, glossary; relative links
  made absolute to GitHub). Its integration test fails when the checked-in JSON doesn't match the file;
  `UPDATE_DESIGN_LESSONS=1 cargo test -p design-lessons` writes it. CI runs it with the workspace's tests, so the
  two can't be merged out of step. A lesson's key is its title as a slug without its number, so renumbering keeps a
  reader's progress; retitling a lesson starts it fresh.
- The pod holds the course as four collections (`construct_tracks`, `construct_lessons`, `construct_cards`,
  `construct_glossary`), each with a `fetch` filler (`extract: json`, `mode: replace`, keyed) reading the JSON from
  `main` daily and when the course is opened. The reader's own fields (done, the day it was done) are no filler's,
  so they survive refills. The course's layouts are the pod's own, shared with `share_publicly` with
  `new_rows: publish`, so a new lesson reaches the public page without a new review; an edit to the layouts still
  needs one.

Nothing in the server or the clients changed for this: the course is Collections, fillers and layouts a pod already
has. Not done: the course on other pods (they could adopt a publication of its layouts and point the same fillers at
the file).
