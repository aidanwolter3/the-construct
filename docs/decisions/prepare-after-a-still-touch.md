# A touch still for 30 ms prepares what its tap opens, whatever node it is on

Date: 2026-11-15. Related: [open-while-pressed](open-while-pressed.md), [android-frame-pacing](android-frame-pacing.md).

Preparing on the pressed frame (decision `open-while-pressed`) measured badly on a phone. Scrolls start with the
finger on a row, so each one first prepared that row's screen (12–20 ms) and its first drag frames came 40–64 ms
apart; the Construct Tasks rows open screens whose data loads in the background, so the work was thrown away every
time. And home's own buttons were never prepared: Construct Tasks opens from a `core://IconButton`, and the tiles'
middles are icon buttons inside their `core://Tappable`, while only a pressed Tappable was looked at.

Decision: the window prepares once a touch has rested 30 ms without starting a drag (`PREPARE_AFTER`, a timer
beside the long press's), with `Session::prepare_at(x, y)`: any node under the point whose tap is
`action.navigation.open`, pressed or not. An open to a screen that loads data (or a layout opened with a screen of
its own) is skipped before any work. Every new touch drops what an earlier one prepared; the rest is as decided
before (the tap with the same intent brings it in, a release that isn't a tap or new data drops it).

A scroll that is still at 30 ms still pays for one layout, and a tap lifted within about 30 ms plus the layout's time
gets no head start (it opens as before). The 30 ms needs checking on a phone.
