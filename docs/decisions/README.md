# Decisions (one file each)

New architecture decisions go here, one file per decision, named after its topic: `docs/decisions/<name>.md`.
D1–D52 stay in [`../DECISIONS.md`](../DECISIONS.md) under their numbers; nothing new is added there.

Why: decisions used to be numbered entries appended to one file. Every PR took "the next number" and added to the
same end of the same file, so parallel PRs always conflicted and had to renumber their decisions and every
reference to them. Separate files named by topic don't touch each other, and there is no number to take.

## Rules

- **Name:** lowercase words joined by hyphens, specific to the decision, e.g. `pixelfed-notification-polling.md`,
  not `notifications.md`. If git reports that the file already exists (an add/add conflict), rename yours.
- **Refer to it** by name: "decision `pixelfed-notification-polling`", or link the file. Don't give it a number.
- **Don't rewrite** a recorded decision. To change course, write a new one, and add one line at the top of the old
  one (or under an old `DNN` heading): `Superseded by [<name>](<name>.md).`
- **Renames are the exception:** when something a decision names is renamed (an app id, a folder), update the
  name in the old decisions too, so a search finds them, and add a row to [Renamed](#renamed).
- **No index:** this folder's file list is the index (`ls docs/decisions`). A shared list would conflict again.
- `scripts/check_decisions.sh` (run in CI) fails if `DECISIONS.md` gets a new numbered entry or a file here has
  a bad name or no `# ` title.

## Renamed

Names older decisions may still use, and what they are now.

| Was | Now | By |
|---|---|---|
| app `watcher`, crate `app-watcher` | app `events`, crate `app-events` | [rename-apps-and-folders](rename-apps-and-folders.md) |
| app `pixelfed_home` | app `pixelfed` | [rename-apps-and-folders](rename-apps-and-folders.md) |
| app `pixelfed_demo` ("Pixelfed demo") | app `pixelfed_sample_no_login` | [rename-apps-and-folders](rename-apps-and-folders.md) |
| `layouts/<app>/` | `crates/<adapter>/layouts/<app>/` | [rename-apps-and-folders](rename-apps-and-folders.md) |
| `kmp-app/` | `app/` | [rename-apps-and-folders](rename-apps-and-folders.md) |
| `fixtures/` | next to the code that reads each file | [data-next-to-its-code](data-next-to-its-code.md) |

The decision files [watcher](watcher.md) and [watcher-rules-per-site](watcher-rules-per-site.md) keep their names:
other decisions link to them.

## Template

```markdown
# <What was decided, in one line>

Date: YYYY-MM-DD. Related: D46, [other-decision](other-decision.md).

<The problem or ambiguity, and what was considered.>

Decision: <what we do>.

<Consequences, limits, and what is not done yet.>
```
