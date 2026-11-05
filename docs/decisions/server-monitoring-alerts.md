# Server monitoring alerts: a notification when a check turns to warning

Superseded in part by [notification-owner](notification-owner.md): the channel belongs to the one layout that shows the source.

Date: 2026-11-04. Related: D52, [server-monitoring](server-monitoring.md),
[collection-reminders](collection-reminders.md).

The `monitor` source shows the server's checks as ok or warning, but only when someone opens an app on it. The
operator wants to hear about a warning (a burst of wrong pod codes, a new Claude connection, heavy model use, low
disk) without looking. Considered: Grafana alerts on the Prometheus page (needs an outside service and its own
setup), and notifications from the pod, which already delivers Web Push to the operator's devices (D52).

Decision: the `monitor` adapter notifies like Collections' reminders, for the pod's custom apps. In a pod with
monitoring on (`monitor.enabled`), every custom app whose feeds name the `monitor` source gets a channel
`server_warnings` ("Server warnings", high importance, on by default). The adapter polls every 5 minutes (only while
the pod has a registered device, as every D52 poll), works out the checks, and sends one notification per app for
each check that warns now and didn't at the last look: "Server: <check>" with the check's numbers as the body. The
ids that warned are kept in the pod setting `monitor.warned`, so a warning that lasts notifies once, and a check
can notify again only after it has been back to ok. The badge text for a warning is "Warning" (was "Look").

Limits: checks are looked at every 5 minutes, so a warning can arrive up to 5 minutes late (a burst between looks
still counts: the windows are an hour or a day long). Counters restart with the server, so a
warning ends at a deploy and may notify again if the cause is still there. Each pod with monitoring on is notified
on its own.
