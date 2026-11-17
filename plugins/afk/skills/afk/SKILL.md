---
name: afk
description: The user is away from the keyboard. Keep working on the current task (or the one given as arguments) without asking them anything, make as much progress as possible, then finish with a summary of the progress and the current state.
argument-hint: "[task or goal to work on while away]"
disable-model-invocation: true
---

# AFK mode

The user has stepped away and will not answer until they come back. Your job is to get as far as you
can on the task without them, and leave them a clear picture of what happened when they return.

**Task:** $ARGUMENTS

If no task is given above, continue the work already under way in this conversation, toward the goal the
user last stated. If there is no such work, say so in one line and stop.

## While working

1. **Don't wait on the user.** Don't ask questions or pause for confirmation: nobody will answer. When a
   decision is the user's to make, pick the most reasonable option, note it as an assumption for the
   summary, and keep going. Prefer choices that are easy to change later.
2. **Plan, then work through it.** Break the goal into steps (a task list helps) and work through them in
   order. When a step is done, go straight to the next.
3. **Keep moving when blocked.** If one path is blocked (a missing credential, a failing dependency, an
   unclear requirement), record the blocker, try a reasonable alternative, and if there is none, move to
   another part of the task that isn't blocked. Only stop when every remaining step is blocked or done.
4. **Verify as you go.** Build, run the tests, and check the result the way the project expects (its
   CLAUDE.md / CONTRIBUTING rules still apply). Don't call something done that you haven't checked; fix
   what you break.
5. **Save work often.** If the project is a git repository, commit coherent steps on the working branch
   as you go, and push to it if this environment is ephemeral, so nothing is lost if the session ends.
6. **Wait without polling.** For long builds, CI or deploys, use background commands, Monitor, PR events
   or a scheduled wake-up rather than sleep loops.

## Stay within what the user would expect

Being away is not permission for more than they already gave. Without explicit, prior approval in this
conversation or the project's instructions, do **not**:

- take hard-to-reverse actions: force-push, rewrite shared history, delete branches, data or files you
  didn't create, drop databases;
- act outward: merge or deploy to production, post comments, send messages or email, open PRs or
  issues, publish anything;
- spend money or change accounts, credentials, permissions or environment settings;
- widen the scope into unrelated work (note it as a follow-up instead).

If the next step needs one of these, stop that line of work, prepare everything up to it, and list it in
the summary as waiting on the user.

## When done

Stop when the task is complete, or when everything left is blocked or needs the user. Then end your
turn with a summary for someone who has just come back and hasn't read the transcript:

```markdown
## AFK summary

**Goal:** <one line>
**Status:** Done | Partly done | Blocked — <one line why>

### Done
- <what was accomplished, with files, commits, PR links>

### Current state
- Branch / commits / pushed or not; uncommitted changes
- Checks: <tests, lint, build — passed, failed (with the failure), or not run and why>
- Anything still running (CI, deploys, background jobs)

### Decisions I made for you
- <assumption or choice> — <why; how to change it>

### Needs you
- <blockers, approvals, questions, device checks>, most important first

### Next steps
- <what remains, in order>
```

Keep it honest and short: report failures and skipped steps plainly, and leave out sections with nothing
in them. If a push-notification tool is available, send a one-line notice that the AFK run has finished.
