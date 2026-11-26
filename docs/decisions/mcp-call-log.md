# The pod logs every MCP call by credential, and the owner can undo a morning's background work in one tap

Date: 2026-11-26. Related: [mcp-grants](mcp-grants.md), [edit-groups](edit-groups.md), [anticipate](anticipate.md),
[agents-cap-v3](agents-cap-v3.md).

Grants (decision `mcp-grants`) limit what an unattended job may do, but nothing recorded what it tried. An injected
email that keeps pushing Sense against its grant looks, from outside, like a quiet morning. And when a morning's
work is wrong, the owner had to undo it piece by piece: each edit group in Settings, each row through Claude.

Considered: logging in the agent runner (the agent's own sandbox writes it, so a steered agent could leave things
out); logging the arguments and answers too (rows hold the owner's mail-derived text, and the sources keep their
own history of what was written); a Settings view of the log now (it needs a client build; the MCP tool and the
warning cover the need first).

Decision:

- **The call log.** The pod records every MCP tool call a credential makes, in its own store where no tool writes
  (`pod_core::call_log`, table `mcp_calls`): time, the credential's name, whether it has a grant, the tool, one
  short target (the first text of the arguments' `id`, `collection`, `component`, `group`, `edit`, `name`,
  `layout`), and the outcome: allowed, refused (by its access or grant: `grant_refuses`, `access_too_low`,
  `protected`, `new_host`, `not_safe`, `owner_keeps`, `owner_only`) or failed, with the error code. Two things of
  an answer are kept because undo needs them: a row edit's id and the checkpoint an applied edit group took. Kept
  30 days. The name is the credential's label, sent by the host as `x-construct-mcp-label` and stripped from every
  incoming request like the other host headers, so a client can't name itself.
- **`call_log {since?, credential?, refused?, limit?}`** (MCP, read): totals per credential since a time (default
  the start of today in the pod's zone) and the calls themselves, newest first.
- **A refusal warning.** When a granted credential is refused 3 times within an hour, the Agents source's timer
  sends a "Refused: <credential>" notice on Runner warnings, once a day per credential, pointing to `call_log`
  and to Pause background agents.
- **Undo this morning.** `undo_background {credential?, since?, dry_run?}` (MCP, edit; owner only) and `POST
  /v1/background/undo` (Settings' manifest, for a button such as Today's summary card): for every credential with
  a grant (or the one named), since a time (default the start of today), restore the checkpoint taken before the
  first edit group they applied, then undo every row edit they made, newest first, with the source's `undo_edit`.
  A granted credential is refused (`grant_refuses`) even with the tool in its list. The dry run says what would
  move, including the owner's own screen edits made since the checkpoint, which the restore also undoes; a
  button asks first. A second undo starts after the first, so a second tap restores nothing.
- **Pause background agents.** `agents_schedules {op: pause_all}` and `POST /v1/agents/schedules/pause_all` pause
  every enabled schedule and drop what waits to be sent. Nothing is signed out or revoked, runs going now go on,
  and each schedule resumes on its own. The Agents core layout (version 9) shows the button above the schedules
  while any is on (`agents://runner/status#can_pause_all`), and asks first.

The controls use existing intents (`action.confirm` and `action.mutation.optimistic`), so no client build. A view of
the log in Settings can come later.
