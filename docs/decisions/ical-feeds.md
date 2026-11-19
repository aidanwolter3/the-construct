# Calendar feeds: a private .ics URL of chosen rows, rendered by the pod and served by the host

Date: 2026-11-18. Related: [public-web-links](public-web-links.md),
[public-links-share-the-closure](public-links-share-the-closure.md), [sharing](sharing.md),
[collection-views](collection-views.md), [collection-reminders](collection-reminders.md).

People want rows of their collections (focus days, workouts, the events they are going to, a one-off event from an
email) in the calendar on their phone. Proton Calendar has no API and no CalDAV; it can only subscribe to an `.ics`
URL ("Add calendar from link") and fetches it from Proton's servers every few hours. So the pod has to serve a feed
that a server it doesn't know reads without signing in.

Considered:

- **CalDAV**, or writing into the calendar: Proton offers neither. Rejected.
- **A route on the pod**, rendering the feed when it is fetched. Always current, but every fetch reaches the pod,
  against "privacy is storage" (decision `sharing`), and it would need a credential in the URL that the pod checks.
  Rejected, as for public links.
- **A copy on the host, rendered by the pod**, the shape of public links. Taken.

Decision: **a pod may keep up to five named calendar feeds. Each maps rows of collections to events; the owner
reviews every text a subscriber would read before a feed goes out; the pod renders it as RFC 5545 and copies the
text to the host whenever its rows change; the host answers it at `GET /ical/<token>.ics` to anyone with the URL.
Only where the host serves public links (`CONSTRUCT_PUBLIC_WEB=1` with `CONSTRUCT_WEB=1`, admin pods).**

- **The core knows no source.** `pod_core::ical` keeps the definitions (the pod setting `calendar_feeds`: name,
  title, sources as given, token), runs the MCP tools, writes the iCalendar text and keeps feeds current. A feed's
  `sources` go to the source that answers `Source::calendar_events` (Collections, `source_collections::calendar`),
  which checks them (a refusal names the JSON pointer) and maps each row to an event: a key, title, location,
  description, start and end, and the row's created and updated times. Nothing else of a row exists in an event, so
  no other field can reach the feed.
- **A source** is `{collection, where?, start, end?, time?, end_time?, duration_minutes?, title, location?,
  description?}`. `where` is a view's (decision `collection-views`): `{field, equals}` or a list, all matching, as
  text. `start` and `end` are date or datetime fields; `time` and `end_time` text (or choice) fields read as
  "8:00 AM", "8am", "15:45", "09:00" or "noon"; `duration_minutes` a number or a number field. `title`, `location`
  and `description` are templates (`"{area} day"`); a title and a location are one line, a description keeps its
  lines. Fields are named, so a renamed field's alias keeps a feed working.
- **Times**: a date with no time is an all-day event (`VALUE=DATE`, `DTEND` the day after its last day). A
  datetime with an offset is that instant; a datetime without one, or a date with a `time`, is read on the wall
  clock of the pod's time zone (`pod_core::tz`, daylight saving included). Moments are written in UTC (`…Z`), never
  floating, so no `VTIMEZONE` is needed; `X-WR-TIMEZONE` names the pod's zone for apps that show it. A timed event
  ends at its `end`, else its `end_time` that day (the next day when it is earlier on the clock), else after its
  `duration_minutes`, else an hour after it starts.
- **Updates in place**: each event's `UID` is `<collection>-<row id>@<hash of the pod id>.calendar.the-construct`,
  the same across edits, rotations and feeds, so an edited row changes the subscriber's event instead of adding
  one, and the pod's id doesn't show. `DTSTAMP` and `LAST-MODIFIED` are the row's `updated_at`, `CREATED` its
  `created_at`, `SEQUENCE` the seconds between them (it only grows). The calendar says `X-WR-CALNAME`,
  `REFRESH-INTERVAL` and `X-PUBLISHED-TTL` of `PT1H` (Proton may ignore them). Lines end in CRLF and fold at 75
  octets, never inside a character; TEXT escapes `\`, `;`, `,` and newlines.
- **Window and size**: events that ended in the last 30 days and everything ahead, the earliest 3000, and only as
  many of those as fit in 2 MiB of text (the host keeps it outside every pod's storage quota, as a public link's
  copy); the review and the count say how many go out. At most 10 sources per feed.
- **Review**: `set_calendar_feed` without `confirm` answers the distinct titles, locations and descriptions every
  event would carry, the event count, the pod's zone and the next ten events, and publishes nothing. `confirm` must
  repeat the review id, a hash of the definition and the rendered text: anything that changed asks again. Changing
  a live feed keeps its token.
- **The host** keeps the text in `host.db` `calendar_feeds` (migration `calendar_feeds`) under `cal-` and 32 base32
  characters, served `text/calendar; charset=utf-8`, `no-cache`, `noindex`. With `CONSTRUCT_PUBLIC_WEB` unset the
  tools aren't listed and `/ical/…` answers 503 as public links do; the stored texts stay.
- **Refresh**: each pod renders its feeds again three seconds after the pod's own data settles (every data
  event without a session), and every hour (the 30-day window moves, the time zone may change), and copies only a
  text that changed. A feed that can't be rendered (its collection or a field it reads went) keeps its last copy.
- **Revoke and rotate**: `revoke_calendar_feed` takes the text off the host at once (the row stays, revoked, so a
  token never answers again) and forgets the feed; `rotate_calendar_feed` puts the feed under a new token and
  revokes the old one. A deleted pod takes its feeds.
- **One-off events** (one from an email) go in a plain collection the user's Claude makes (title, start, end,
  location, notes) and adds rows to with `add_row`, listed as a source of the feed. Blueprints don't ship one.

A collection filled from the same calendar (an `ics` or calendar MCP filler) shouldn't be a source of its feed, or
its events come back twice; a `where` keeps them out.

Not yet: recurring events (`RRULE`) and alarms; `TZID` with `VTIMEZONE` (UTC is exact); per-feed fetch limits;
a feed of data other than Collections'.
