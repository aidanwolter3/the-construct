# Staging's image is built on the CI runner, not Fly's remote builder

Date: 2026-11-27. Related: [browser-sandbox](browser-sandbox.md), [device-versions](device-versions.md).

From 2026-11-26 most deploys of `main` to staging failed while Fly's remote builder compiled the server
(`cargo build --release` in `deploy/pod.Dockerfile`, thin LTO, one codegen unit). Either rustc was killed with
SIGKILL while compiling `pod-core` (out of memory: #559, #560, #563, #564), or the build froze at `pod-host` until
the deploy job's 30 minute limit (#557, #566, #567, #569). Only #568 and two manual re-runs got through. None of
those commits changed the build; the workspace crates had just grown past what the builder has room for. The web
client's build already waited for the server's for the same reason.

Measured on 4 cores and 15 GB (the size of a GitHub runner), the server's release build takes 4 to 5 minutes; the
largest rustc processes peak at about 1.6 GB (`server`, `pod-core`), 1.2 GB (`source-collections`) and 0.8 GB
(`construct-render`), and the most running at once (`pod-core` with five sources) about 5.3 GB.

Decision: the deploy job builds the image on its own runner (`flyctl deploy --local-only`) and pushes it to Fly's
registry. The runner has 16 GB and runs nothing else, so the build has three times its peak. Nothing is lost to
caching: the remote builder rebuilt the dependency layer (`cargo chef cook`) on every deploy anyway.

- **Rejected: fewer jobs on the remote builder** (`cargo build -j 2`). It would lower the peak, but the builder's
  memory isn't ours to see or size, and it was already the cause once before.
- **The fake Pixelfed** (`fake-pixelfed.yml`) keeps the remote builder: its image is small.
