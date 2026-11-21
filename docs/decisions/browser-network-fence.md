# The headless browser's user may connect only to its proxy

Date: 2026-11-21. Supersedes [browser-private-network](browser-private-network.md). Related:
[browser-sandbox](browser-sandbox.md), [rendered-fetches](rendered-fetches.md), [server-monitoring](server-monitoring.md),
issues #448, #318.

The nftables rule of decision `browser-private-network` kept the browser's user (`nobody`, uid 65534) off Fly's
private network (`fdaa::/16`) only. Native code running as that user after a renderer escape, or under the
`--no-sandbox` fallback, could still reach the server's own port on loopback (and forge `Fly-Client-IP` there,
which the server trusts on Fly, to dodge rate limits), the metrics port, the VM's private IPv4 and link-local
addresses, and perhaps Fly's `/.fly/api` socket. The browser needs exactly one connection: to its proxy on
loopback. It resolves no names and sends no UDP.

Decision:

- **Allow one connection, refuse the rest.** The table `inet construct_browser` sends every packet of uid 65534
  to a chain of its own, which accepts `127.0.0.1:1009` (the proxy), accepts TCP on `lo` that isn't a lone SYN
  (answers on connections opened *to* the browser: the server's DevTools connection), and rejects everything
  else at once (`icmpx admin-prohibited`). It is stateless, so it needs no connection tracking.
- **The proxy's port is fixed while fenced.** One render at a time, so one proxy: on `127.0.0.1:1009`
  (`render::PROXY_PORT`). Below 1024, so the browser's own user can't take it first. Without the rule (not root,
  tests) the proxy takes any free port as before.
- **No unfenced, unsandboxed renders on Fly.** If the rule didn't load on a Fly machine and Chromium's sandbox
  doesn't work either, the server renders no pages (rendered fillers say so; the log says `browser sandbox: off
  ... the server renders no pages`). It still serves everything else. Off Fly, the old fallback stands.
- **Fly's API socket.** At startup a root server checks `/.fly/api`; if other users may connect to it, it takes
  that away (root keeps it) and logs it.
- **Metrics stay on `[::]:9091`.** The fence already refuses the browser's user there. Binding them to the 6PN
  address only (`fly-local-6pn:9091` in `deploy/fly/pod.toml`) would also keep them off loopback for every other
  local user; it is a deploy change, left for its own PR.
- The log lines stay: `browser private network block: on` (now naming the proxy) or a `... off` warning.

Consequences: no layout vocabulary and no client build. The server itself still runs as root (volumes mount
root-owned, and the rule needs root at startup); dropping its privileges after setup is left for later (#448).
A separate browser machine or network namespace stays possible.

Not yet: checked on a Fly machine (`fly ssh console`; steps in [docs/server.md](../server.md) "Deploying").
Checked in a cloud session as root in a network namespace: `nobody` reaches `127.0.0.1:1009` but not `:8080` or
`:9091`, and a real Chromium render through the fence works.
