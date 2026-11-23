`core://Carousel` `tap_paging: true` (decision `scatter-and-tap-paging`): pages turn on taps, not swipes. A
tap on the left third shows the previous page and on the right third the next, wrapping round; `on_tap` runs on
a tap in the middle third. Dots over the page show where it is. A sideways drag is left to what is around the
carousel (a `core://SwipeCard`). Every page's images load as soon as the carousel is drawn, and a page whose
image hasn't arrived yet waits (at most a moment) with the current one showing, so paging never shows a blank.
