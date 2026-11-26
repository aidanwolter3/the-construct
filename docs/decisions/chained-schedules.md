# A schedule can follow another: it starts when the other's run ends, with its own cron as the fallback

Date: 2026-11-26. Related: [agents-cap-v3](agents-cap-v3.md), [anticipate](anticipate.md),
[mcp-call-log](mcp-call-log.md).

Anticipate is two runs (decision `anticipate`): Sense reads the day and writes needs, then Build reshapes the screens
from them. They ran at fixed times half an hour apart, so Build either waited idle after a quick Sense or started
before a slow one finished and worked from yesterday's needs.

Considered: a `then` field on the upstream that starts named schedules (the follower's own definition would no longer
say when it runs); having the runner chain runs (the runner keeps no schedules since CAP v3, and the pod owns work
items); starting the follower on a successful end only (a failed or timed-out Sense still leaves Build useful work,
such as undoing yesterday's trial; Build reads whatever needs exist).

Decision:

- **`after`** on a schedule names the schedule it follows (`agents_schedules` column `after_schedule`; `none` clears
  it). It must name a schedule that exists, not itself, and not one that follows it, however far along.
- **When it starts.** On each timer pass, for a schedule that is on and follows another: take the other's newest
  occurrence from its cron (not Run now) that the runner answered with a run. Once that run has ended in the mirror,
  however it ended, the follower's first cron time after that occurrence, if it is within 24 hours and still to come,
  is taken up now and sent like any due time.
- **Once a day.** The early start is that cron time's own occurrence, with the same `request_id`, so when the cron
  reaches the time the occurrence is already taken up and nothing more is sent. The cron is the fallback: when the
  other never ran or its run never ended, the follower runs at its own time.
- **Renames and deletes.** Renaming a schedule carries its followers along; deleting one leaves them only their own
  times.

No new intent, node or field on a screen, so no client build. Anticipate's Build follows Sense, with 06:30 as its
fallback.
