# Protected paths need the developer's review; deploy and signing secrets live in Environments

Date: 2026-11-24. Related: issues #437, #441, #451, #452, [ci-container-image](ci-container-image.md).

`main` had no branch protection, auto-merge was on, and a push to `main` deploys staging, which holds real pods,
their Pixelfed tokens and the shared model key. The Fly deploy tokens and the Android signing key were repository
secrets, so any run on any ref could read them: a pushed branch could add a workflow with `on: push`, or dispatch
`release.yml` from itself. Agent sessions push branches and merge their own PRs on green CI, and CI runs code the
PR controls, so one bad PR (or a prompt-injected session) could take staging or the signing key.

Considered: requiring the developer's review on every PR (safe, but it stops agents from landing ordinary work), and
leaving it to the `ship` skill's text (agents follow it, but nothing enforces it).

Decision: the developer reviews only the paths that run CI, deploy, sign or steer agents; the secrets move where
only `main` and release tags can read them.

- **`.github/CODEOWNERS`** lists `.github/`, `deploy/`, `plugins/`, `.claude/`, `.claude-plugin/`, `CLAUDE.md`,
  `scripts/android_sign.sh` and `scripts/cloud_setup.sh`, owned by the developer. Branch protection on `main`
  requires the `rust`, `audit`, `android` and `web` checks, a pull request, and Code Owner review, so a PR touching
  those paths waits for the developer's approval and everything else still merges on green.
- **Environments**: the deploy jobs (`ci.yml` `deploy`, `fake-pixelfed.yml` `deploy`) run in `staging`, deployable
  from `main` only, which holds `FLY_API_TOKEN_POD` and `FLY_API_TOKEN_FAKE`. The signing job (`release.yml`
  `android-sign`) runs in `release` (tags `v*` and `main`, the developer as required reviewer), which holds the
  four `ANDROID_KEY*` secrets; a pull request's run uses no Environment and signs with a debug key.
- **Tokens**: `FLY_API_TOKEN_POD` is a deploy token (`fly tokens create deploy -a the-construct-pod`), which can't
  open `fly ssh console` to read pod databases or the server's keys.
- **The `ship` skill** opens, greens and waits on a protected-path PR, and merges it only after the owner approves;
  it never approves a PR itself.

The settings half (branch protection, the Environments, moving the secrets, the deploy token) is the developer's
in GitHub and Fly settings: a session can't change them. Until it is done the files here change nothing: the jobs
use Environments GitHub creates on first use, with no rules, and still read the repository secrets.
