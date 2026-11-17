# The headless browser runs sandboxed where it can, and its Chromium is rebuilt weekly

Date: 2026-11-16. Related: [rendered-fetches](rendered-fetches.md), issue #292 (after the audit fix #291).

Rendered fillers load untrusted pages in Chromium inside the server's VM, where every pod's data lives. The browser
already runs as `nobody` with an empty environment, so a compromised renderer can't read the keys or the pods'
files, but it ran with `--no-sandbox` and with site isolation off, so a renderer exploit ran native code on the VM
(and could reach Fly's private network). And the image's Chromium was never refreshed: Debian's `chromium` was
installed in a layer the builder kept until the Dockerfile or the base digest changed.

Whether Chromium's sandbox works on Fly's kernel (unprivileged user namespaces) can only be checked on a machine
there. Shipping the sandbox unconditionally would break every render if it doesn't; keeping `--no-sandbox` until
someone checks would keep the gap open.

Decision:

- **Sandbox first, fall back once.** The server's first render starts Chromium with its sandbox and site isolation,
  and checks that a tab can run script (a sandbox that lets the browser start but not its renderers fails there).
  If either fails, that render and every later one until the server restarts runs as before: `--no-sandbox`,
  `--disable-site-isolation-trials`, `IsolateOrigins,site-per-process` off. Site isolation goes with the sandbox:
  without it, it protects little and costs a renderer per site on a 1 GB machine. The outcome is logged once,
  `browser sandbox: on` (info) or `browser sandbox: off` with the reason (warning), target `browser`. If neither
  mode starts, nothing is kept and the next render tries again.
- **Chromium rebuilt weekly.** `deploy/pod.Dockerfile` has `ARG CHROMIUM_REFRESH` before its apt `RUN`, which also
  runs `apt-get upgrade` (Debian's security updates, for a base digest older than them). CI's deploy passes the ISO
  week (`date -u +%G-W%V`), and `ci.yml` also runs on Mondays, so the layer is rebuilt at least weekly, push or not.
  No bot PR: a weekly commit that only bumps a date would need its own review and CI run; a build argument doesn't.

Consequences: no layout vocabulary and no client build. The fallback never retries the sandbox before a restart (each
deploy restarts). The probe costs one call timeout (15 s) on the first render if renderers hang under the sandbox.
A manual `flyctl deploy` without the build arg reuses whatever layer the builder cached for `unset`.

Checked on staging (2026-11-17): its log reads `browser sandbox: on (Chromium's sandbox and site isolation)`, so
Fly's kernel allows the sandbox and staging doesn't fall back.

Not yet: running the browser in its own Fly machine without the private network (issue #318); base-image digest
updates for `deploy/pod.Dockerfile` (the weekly rebuild takes Debian's updates without them).
