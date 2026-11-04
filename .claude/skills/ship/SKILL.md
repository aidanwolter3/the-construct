---
name: ship
description: Take a change from a branch to merged on main and deployed to staging - sync with main, run the checks, open the PR from the template, watch CI, fix failures, merge when green, confirm the deploy. Use whenever the user says "ship it", "land it", "open/make/send a PR", "merge it once checks pass", "watch it and merge", or "fix CI and merge"; per CLAUDE.md, a request for a PR means this whole loop unless the user says not to merge.
---

# Ship: branch → PR → green → merged → deployed

Do the steps in order. The user asked for the outcome, not for each step: don't stop to ask between them unless
a step below says to.

## 1. Sync with main and check

1. `git fetch origin main`, then `git merge origin/main` into the branch (a merge, not a rebase, so no
   force-push is needed). Resolve conflicts:
   - **Layout files** (`crates/<adapter>/layouts/<app>/NNNN.json` add/add): each file is a whole snapshot. Merge both layouts into
     the next free number, keep main's file unchanged, and re-run the adapter's tests. Never edit a file main
     already has.
   - **Migration lists** (named migrations, see docs/decisions/named-migrations.md): keep both entries, main's
     first. Never rename a migration that main already has.
   - **Docs, decisions**: keep both sides' content.
2. Run the checks in CONTRIBUTING.md "Checks". In a cloud session run `scripts/await_warmup.sh` before the JVM
   tests. For an interface change, take simulator screenshots (docs/simulator.md).
3. Commit (CONTRIBUTING.md commit style) and `git push -u origin <branch>`.

## 2. Open the PR

1. Fill in `.github/pull_request_template.md`. **New or removed feature** and **Client update needed** are
   never left as "no" by default: decide them from the diff. A new node type, intent, field, or engine store
   change means a client update.
2. Link the issue: `Fixes #N` when this PR finishes it, `Part of #N` when the issue plans more PRs.
3. Screenshots: push the PNGs to the side branch `screenshots/<branch>` (never merged) from a separate
   worktree, and embed them by raw URL:
   ```bash
   git worktree add --detach /tmp/shots && cd /tmp/shots
   git checkout --orphan "screenshots/<branch>" && git rm -rfq . && cp /tmp/sim/*.png . && git add *.png
   git commit -qm "Screenshots for <branch>" && git push -u origin "screenshots/<branch>"
   cd - && git worktree remove --force /tmp/shots
   ```
   `https://raw.githubusercontent.com/aidanwolter3/the-construct/screenshots/<branch>/<file>.png`
4. Create the PR (GitHub MCP `create_pull_request`).

## 3. Get it green

1. Turn on auto-merge (`enable_pr_auto_merge`, squash). If GitHub says auto-merge is disabled for the repo, carry
   on: you merge it yourself in step 4.
2. `subscribe_pr_activity` for the PR, and schedule one backup check-in about 30 minutes out (`send_later`) in
   case an event is missed. Don't poll with `sleep`; between events, end the turn or use Monitor.
3. On a red check: read the job log, reproduce the failure locally, fix it, run the checks again, push. Never
   skip, ignore or loosen a test to get green. Sessions can't re-run a job (the API answers 403): for a flaky
   failure, fix the flake (or file it with the `ticket` skill) and push. If main moved underneath, merging it
   in also starts a fresh run.
4. When main moves while you wait (GitHub reports the branch behind or conflicting), repeat step 1 before merging.

## 4. Merge and confirm

1. If auto-merge didn't merge it: when every check is green, squash-merge with `merge_pull_request`, passing the
   head SHA in full (`git rev-parse HEAD`, 40 characters; a short SHA is rejected).
2. `unsubscribe_pr_activity` and delete the backup check-in.
3. Confirm the push to main: its CI run, the staging deploy job, and for pod changes
   `curl -fsS https://the-construct-pod.fly.dev/healthz`. If the deploy fails, root-cause it and fix it as a new
   PR, going through this skill again.
4. Start any later work on a fresh branch from `origin/main` (`git checkout -B <branch> origin/main`, then
   `git push --force-with-lease -u origin <branch>`). Never stack work on merged history.

## 5. Report

One short message, outcome first:
- the PR link and that it merged (or what's blocking it);
- **Client update needed:** yes or no, in plain words ("you need a new Android/desktop build" or "ships with the
  server");
- device checks that only the developer's Mac or phone can do;
- for new pod MCP tools: the user must refresh the connector in Claude's connector settings ("Refresh tools
  list") or start a new chat.

## Don't merge when

- the user said not to, asked for a draft, or wants to review first;
- the PR isn't this session's (another session or person owns it);
- a check is still red or pending, or a required screenshot is missing.

Then stop after step 3 and say what's left.
