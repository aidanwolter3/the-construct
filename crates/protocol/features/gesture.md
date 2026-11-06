`core://Gesture {content, on_tap?, on_long_press?, on_swipe_up?, on_swipe_down?, on_swipe_left?, on_swipe_right?,
content_description?}`: `content` with gesture handlers, each running an intent (decision
`edge-overlays-and-gestures`).

Fields: content, on_tap?, on_long_press?, on_swipe_up?, on_swipe_down?, on_swipe_left?, on_swipe_right?, content_description?.

content with gestures, each running its intent (at least one): a tap, a long press, or a swipe up, down, left or right (a drag mostly that way). A pull the content can't scroll for counts as a swipe, so a page inside still scrolls and pulling it down at its top is a swipe down (e.g. on_swipe_down opens the inbox as an Overlay from edge top). core://Tappable is the tap case.
