# The owner's taste history lives in the pod; the taste service caches it

Date: 2026-11-25. Related: [taste-data-source](taste-data-source.md), [jacks](jacks.md).

The taste service learns what the owner likes from every like and dislike, and it was the only place those were
kept (`data/state.json` on its machine). The pod held the deck and its newest 60 reactions, for their feed reasons.
Losing the service's disk would have lost the owner's whole taste, and the history belonged to the service, not to
the owner's pod, unlike everything else the Construct keeps. Considered: the pod keeping a copy of each reaction as
well (two copies that drift, and the service still the one that counts); the pod sending its whole history with every
request (a stateless service; about 10 KB compressed today, growing with every swipe, for every call); and the pod
owning the history with the service holding a cache of it, filled on demand. The owner chose the last.

Decision:

- **The pod keeps the history** in the adapter's tables: every reaction in order (`taste_reactions`), how often each
  piece has been served (`taste_served`), and its name, a random `log` made once per pod (`taste_history`). Signing out
  or unplugging the jack forgets the service and the kept feed, not the history: it is the owner's, and a new service
  is given it.
- **Every request names it:** `X-Taste-State: <log>/<n>`, n the reactions in it. The service answers from its copy
  when it holds exactly that version and 409 `{"error": "state_unknown"}` when it doesn't; the pod then sends the
  whole history (`PUT /api/state {log, events, served}`) and asks again, once. Day to day a request carries a few
  bytes more; a service on a new machine costs one upload.
- **A reaction is the pod's first:** it is added to the history, then sent naming the version it makes (the service
  takes it when it holds one fewer, and answers a repeat of its last one as done). One the service refuses or can't
  be reached for is taken back out, so the swipe is undone as before. Reactions go one at a time (a lock), so the
  version named is the one the pod holds.
- **Served counts are the pod's too:** every page fetched counts its pieces once more, so a rebuilt service keeps
  exploring where the owner hasn't been. They don't change the version: a difference only steers exploration, and the
  next upload settles it.
- **Taking over the history built before:** while the pod hasn't, its requests name no history and the service uses
  its own. On the first request after this ships, the pod reads the service's own (`GET /api/state` without the
  header) and makes it its history, once. A service from before (no `/api/state`) is asked again at most every ten
  minutes and keeps working on its own state meanwhile.
- **Requests without the header** keep using the service's own state, so its web page and pods from before work as
  they did.

Consequences: no client build, and no layout change. The service can be rebuilt, moved or replaced, and one service
can serve several pods, each with its own history. The history is now in the pod's database (and its backups). The
service's web page keeps its own, separate history.
