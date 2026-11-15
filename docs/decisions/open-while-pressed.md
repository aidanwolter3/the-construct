# A held row opens what its tap will open, out of sight, before the finger lifts

Superseded by [prepare-after-a-still-touch](prepare-after-a-still-touch.md).

Date: 2026-11-15. Related: [android-frame-pacing](android-frame-pacing.md), [rust-clients](rust-clients.md).

With taps cheap to handle, opening a screen still dropped one or two frames on a phone: the slide's first frame
laid the new screen out (17–30 ms), on a CPU the scheduler had let slow down while the app was idle. A tap acts on
the lift, and a finger rests on the glass 50–100 ms before it lifts; in that time only the pressed highlight is drawn.

Decision: right after the pressed frame is drawn, while the finger is down and hasn't started a drag, the window
asks the session to prepare the press (`Session::prepare_press`). When the pressed `core://Tappable`'s intent is
`action.navigation.open`, the session runs it, lays out the entries it pushed, and takes them off the stack again,
keeping them with the open's effects. The tap with that same intent puts them back, their slide starting then, with
nothing left to lay out. A release that isn't a tap (the touch became a scroll), another press or new data drops
what was prepared (new data relays it: it is laid out again when it comes in). A screen whose data loads in the
background isn't prepared, since its answer is matched to the stack. A tap that only opened something, with no data
merged and no field left, no longer lays out what is under it again. On Android a touch also tells the performance
hints that work is about to grow (decision `android-frame-pacing`).

The open runs at the press, so a layout the engine hasn't loaded is asked for then; a touch that turns into a
scroll has done that work for nothing, which is a layout's worth of CPU per such touch. Not done: preparing what
other intents do (a write, a sheet's own data), and preparing on a mouse press on the desktop.
