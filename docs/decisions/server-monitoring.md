# Server monitoring: in-memory counters, a Prometheus page and a pod data source

Date: 2026-11-04. Related: D46, [custom-apps-and-data-sources](custom-apps-and-data-sources.md),
[mcp-connector](mcp-connector.md), [pod-storage-limits](pod-storage-limits.md).

The server logged security events (wrong pod codes, rate limits, new OAuth clients and MCP credentials) but nothing
counted them, so noticing an attack meant reading `fly logs`. Model calls (Collections' `model` fields and its
fillers, which read pages and run agent fills with web search) were not counted at all, though they are the one
thing a caller can spend money on: temporary pods can't reach the model, but a stolen MCP credential or pod code for an admin pod could. Options considered: a log
shipper with alerts on log lines (no code, but needs an outside service to see anything), the `prometheus` or
`metrics` crates (more than we need), and our own small registry.

Decision: `pod_core::metrics` is a process-wide registry of counters with fixed label values (no user input, so
few series), every one declared up front so readers see it at 0. The host counts responses by status and its
security events; the model callers count calls by outcome (`ok`, `error`, `refused`) tokens by direction, and web searches and fetches. The
host supplies gauges (live pods, MCP credentials, free disk) measured when read. Two readers:

- `GET /metrics` in the Prometheus text format, on its own listener (`CONSTRUCT_METRICS_ADDR`), never the public
  address. Staging binds `[::]:9091`, which only Fly's private network reaches; Fly scrapes it into its Grafana.
- The `monitor` data source (`crates/app-monitor`, no apps or tables): one feed with an overall status,
  plain-language checks (ok or warn, thresholds in code and `CONSTRUCT_MONITOR_*`) and every counter with its last
  hour and last day, so a custom app shows the server's health at a glance. The data is the whole server's, so it
  is off in every pod until the operator runs `server pods monitoring <name> on` (pod setting `monitor.enabled`;
  admin pods only).

Windows come from copies of the counters kept every 5 minutes for just over a day (`metrics::History`).

Limits: everything is in memory, so a deploy or restart starts counting again (the summary says so for the first
day); Grafana keeps the long history. Counters are per process: fine for one machine, and Prometheus sums them if
there are ever more. The `server pods mcp` CLI runs in its own process, so its credentials show in the gauge but not
in the "issued" counter. Not done yet: notifications when a check turns to warn (the source could poll like
Collections' reminders), and per-pod model use (which pod spends the tokens).
