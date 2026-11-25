# Anticipate: two morning runs keep the owner's journeys working, bounded by the server

Date: 2026-11-25. Related: [journeys-and-dry-runs](journeys-and-dry-runs.md), [edit-groups](edit-groups.md),
[mcp-grants](mcp-grants.md), [usage-trails](usage-trails.md), [agents-cap-v3](agents-cap-v3.md),
[mcp-access-levels](mcp-access-levels.md), [trace-from-home](trace-from-home.md).

The owner wants an agent to run every morning that looks at the day ahead and makes the Construct ready for it. It
should focus on critical user journeys rather than code: what they will need today, which journeys serve that and
where those break, and fixes deployed only when strictly net positive. The design and PRD went through four rounds
of architecture review (design doc "Anticipate: design and PRD").

Considered:

- a model loop inside the pod: the agent module runs bounded fills and has no mail;
- one runner run with subagents per pass: subagents share the session's credentials, so separating them is not a
  boundary;
- a kit that installs collections and a card: kits hold one collection, and a kit's prompt would be one owner's;
- a "morning brief": it drifts from journeys to reminders.

Decision:

- **Runs.** There are two schedules on the agent runner, each in its own workspace with its own pod credential:
  - *sense* (05:07) reads mail and calendar and writes only need rows;
  - *build* (05:37) never sees mail, walks the journeys and changes screens only as edit groups.

  The limits are the server's, through grants, not the prompt's.
- **The primitive.** The journey is a row of the owner's `journeys` collection: an outcome and a script that
  `walk_journey` replays deterministically. Only journeys the owner confirmed count as the regression suite or
  justify an applied change.
- **The gate.** The server enforces part of it: additive, protected, valid, not stale. Build holds the rest:
  traceable, covered, proven by walking the staged group, drawn, a skeptic's yes, and the mode. The owner sets a
  ceiling (`propose` at first); Build steps itself down when changes are reverted or regress. Memory is counters
  read from what the pod holds, never model-written lessons.
- **Surface.** It shows on the owner's Today copy, which is what home reaches. That copy is the owner's layout,
  not a core layout. Today reads as a list to clear:
  - a one-line summary;
  - cards to acknowledge: fixes with Undo, ideas with Apply, questions with two answers, things to know;
  - broken journeys;
  - one to-do list where Anticipate's to-dos sit beside the owner's workouts, lesson, events and errands.

  An answer is a row the next Build run reads. It acts on it within its grant, or turns it into a to-do. The
  owner's goal tiles moved to the Week tab.
- **What ships.** This repo ships the generic pieces and docs/anticipate.md (prompts, setup). The collections, the
  card and the schedules are set up on the owner's pod with the owner.

Not done: repository fixes (they are text for the owner to ticket; Build has no GitHub access); other owners' pods
(the pieces are generic, the prompts are not).

Since then (the owner, the same day): no confirming. Anticipate decides which journeys matter from usage:

- **Established journeys.** A journey walked at least 3 times in 14 days is established. One unused for 30 days
  drops out. Only established journeys form the regression suite.
- **What a fix needs.** A fix needs a need of confidence 0.6 or more, or a regression to repair. Every
  established journey must still pass after it.
- **The mode** is automatic: `propose` for the first 7 days, then `apply`, stepping down after reverts or
  regressions. An owner-written `mode` row only overrides it.
- **What's lost:** the owner's tick no longer stops Build from weakening a script. The prompt forbids it, the
  skeptic sees the diff, and every script change is listed on the card.
- **A starting set.** The pod starts with about 10 journeys assumed important, established from day 1 (the
  owner's call: waiting two weeks of usage was too slow). Build writes their scripts on its first run, and usage
  retires the ones the owner doesn't use.
- **Trust and restyling.** Changes may restyle, not only add, under the server's `safe` check. The first week's
  changes are trials on the real screen (Undo, Redo, Keep), undone the next morning unless kept; after that they
  go live directly. Only Settings is protected.

Since then (the owner, 2026-11-25, after Build's first run changed nothing):

- **Today is shaped every morning.** Fixes wait for something to break, and the first run found nothing broken,
  so it changed no screen. The owner most wants Today fitted to the day and the week, so Build now stages one
  group on Today every morning, quiet day or not: it reorders, regroups, restyles and brings in what other screens
  already bind, under the same server `safe` check. Its skeptic asks "is this a better Today for this owner
  today?", not whether it is the smallest fix. It is always a trial: undone the next morning unless kept, and
  reshaped from the kept base. Shapes don't count toward the 3 fixes or toward stepping back to trials (a daily
  shape undone each morning would otherwise keep Build in trials forever).
- **Home and Today are never protected.** Only Settings is: protecting Today would stop the shape.
- **Dead taps count on the current screen.** A tap's node is resolved against the layout as it is now; only taps
  whose node is gone are dropped. Build tags the tappable nodes it adds or moves so their taps survive edits.
- **The prompt carries the gate.** The schedule's prompt no longer reads `docs/anticipate.md` from a checkout
  (the runner's workspace refused the `git pull`); the gate is in the prompt.

