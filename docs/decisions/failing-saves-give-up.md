# A save the pod keeps failing is given up after 10 minutes, and the device says so

Date: 2026-11-24. Related: D14, [recent-server-errors](recent-server-errors.md).

The outbox (D14) sends rows strictly oldest first and retries a 5xx, 408, 429 or network error forever with backoff
capped at 60 s. That is right for a phone that is offline or a pod that is restarting, but a row the pod answers
with an error every time blocks every later row forever. It happened: a phone's saves stopped reaching its pod for
a day (workout times, a tab bar style, check-offs) while the app showed each one as done, the pod's progress ring and
"Next up" never changed, and nothing on the device or the pod said why. The pod answered one queued request with a
502 (its upstream refused it) about once a minute; only signing out, which drops the outbox, unblocked it.

Considered: skipping a failing row and sending later ones (breaks the order rows of one entity rely on, D14's
rollback re-applies later rows in order); giving up after N attempts (the count depends on the backoff and on how
long the phone was offline); treating an upstream 502 as a 4xx on the pod (hides real outages, and the device would
still never learn why).

Decision:
- **Give up on time, counted from the pod's errors only.** A row gets `failing_since` (unix ms, store step
  `failing_since`) the first time the pod answers it with an error (5xx, 408, 429). Once the pod still answers it
  with an error `EngineConfig::give_up_after` (10 minutes) later, the engine treats that answer as a refusal: the row
  is rolled back as a 4xx would be (D14) and the rows behind it are sent. Network errors never start the clock, so
  a phone offline for a day keeps all its saves.
- **The device remembers what it gave up**, this run only (the last 5; cleared at sign-out), and reports the
  outbox's health in `StateSnapshot::sync_json`: the oldest pending row once an attempt failed (endpoint, short
  error, attempts, `since`) and the given-up rows. The short error is the status and the pod's JSON `error` code
  (`HTTP 502: taste_unreachable`), else the body's first 80 characters.
- **`local://connection` says so**: `stuck` (online, and the oldest change is failing at the pod), `error`,
  `since`, `dropped`, `dropped_text`, and the label gains "· not saving". The core Settings layout (`settings`
  0011) shows why changes aren't reaching the pod and which were undone.

Consequences: a change can now be undone without a 4xx, after 10 minutes of the pod refusing it; the device says
which. Needs a new desktop and Android build (engine store step, snapshot field, renderer). Pods whose home is a
copy of Settings don't get the new rows; their layouts can bind the new `local://connection` fields. The given-up
list is not persisted: after a restart only the undone values remain.
