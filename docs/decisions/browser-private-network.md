# The headless browser's user is kept off Fly's private network with nftables

Superseded by [browser-network-fence](browser-network-fence.md).

Date: 2026-11-19. Related: [browser-sandbox](browser-sandbox.md), [rendered-fetches](rendered-fetches.md), issue #318.

Rendered fillers run Chromium inside the server's own Fly machine, as `nobody` (uid 65534) with an empty
environment and, on staging, with Chromium's sandbox. The loopback proxy filters the browser's HTTP traffic, but a
page that escapes the renderer (or runs where the sandbox fell back to `--no-sandbox`) has native code on the VM,
and the VM can reach Fly's private network (6PN, `fdaa::/16`): other apps' machines, the server's metrics port and
Fly's internal DNS. The browser itself only ever needs loopback, to reach its proxy.

Two ways were open: move the browser to its own Fly app without the 6PN, reached over a narrow API, or keep it on
the VM and block the 6PN for its user. The first is the stronger wall but a new app, deploy and API to run; the
second is a few lines, since the browser already runs as its own uid.

Decision:

- **Block, on the VM.** `deploy/pod.Dockerfile` installs `nftables`. At startup, before any render, a root server
  with a browser configured (`render::block_private_network`) loads one table of its own, `inet construct_browser`,
  with `nft -f -`: in the output hook, `meta skuid 65534 ip6 daddr fdaa::/16 reject`. Every other user, and uid
  65534's loopback connection to the proxy, is untouched. The table is replaced whole each time (`add`, `delete`,
  then the table), so a restart doesn't stack rules. `reject` rather than `drop`, so a connection fails at once
  instead of hanging.
- **Fail-safe.** If `nft` is missing, netfilter isn't there, the rule is refused or `nft` takes over 10 s, the
  server logs a `browser private network block: off` warning with the reason (target `browser`, like `browser
  sandbox: off`) and serves as before. It never stops the server starting. When the rule loads it logs
  `browser private network block: on`. A server that isn't root runs the browser as its own user, so there is
  nothing to fence: it logs that the block is off and why, at info.

Consequences: no layout vocabulary and no client build. The block covers the 6PN only, not other private ranges;
the proxy already refuses those for HTTP, and the browser resolves no names itself. A separate browser machine
stays possible if the sandbox and this rule ever look thin.

Not yet: checked on a Fly machine that uid 65534 can't connect to an `fdaa::` address (needs `fly ssh console`,
which cloud sessions can't do; steps in [docs/server.md](../server.md) "Deploying").
