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
