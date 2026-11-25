# Undo and Redo of an edit group swap the screen on the device at once

Date: 2026-11-25. Related: [edit-groups](edit-groups.md), [anticipate](anticipate.md),
[content-addressed-construct](content-addressed-construct.md), [element-roles-and-reuse](element-roles-and-reuse.md),
[layout-features](layout-features.md).

The owner tries Anticipate's changes by swapping back and forth with Undo and Redo on Today's card. On the pod a
swap is already a pointer move (both elements exist; decision `edit-groups`), but the device only redraws after
the round trip: the mutation, the new root over the events stream, a fetch of the moved element and the new
manifest, then each layout's data. That is a second or more on a phone, too slow to compare two screens.

Considered: having the pod push the other version only after the tap (still a round trip); caching every version
the device has drawn (it never drew the other one before the first swap, and old versions are dropped by the
device's garbage collection); a client-side layout diff (a second layout system, against `one-layout-system`).

Decision:

- The manifest a device is sent lists `swaps` (only for a client that sends feature `construct_swap`): for each
  edit group applied in the last 7 days (a trial, kept or undone; the newest 8), each name it wrote with the
  element it had before and the one the group wrote, both as a manifest's devices are sent them (stripped, so
  re-sealing doesn't move them): `{"swaps": {group: {name: [before, after]}}}`. `POST /v1/objects` serves a
  manifest's swaps with its names, so the device holds both versions from the first fetch, and its garbage
  collection keeps them.
- The intent `action.construct.swap {group, to}` (`to`: `before` or `after`) tells the device to draw those names
  as that element: the engine keeps the swapped names as overrides on the manifest's names, keys the construct
  again from objects it holds, and puts the layouts shown whose element changed straight into its state with the
  data they have, so the next frame is the other screen. Nothing is sent; the layouts' data is then reloaded in the
  background (a swapped element may read other queries).
- The pod is still told, by the Undo or Redo mutation in the same `action.sequence`. Its next root replaces every
  override: the device's swap and the pod's agree, so nothing moves on screen. When the device can't swap (an old
  manifest without the group, an element not here) the intent does nothing and the root draws the change as before.
- A layout using the intent needs feature `construct_swap`; an older build is served the version before it, whose
  buttons only send the mutation.

Not yet: a reload of home that was already on its way when the swap ran can land after it and show the old screen
until the swap's own reload lands (a moment). The swap covers names an edit group wrote; collection changes are
not in groups and are not swapped.
