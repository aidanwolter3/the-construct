`core://SwipeCard {content, on_swipe_left?, on_swipe_right?, threshold_dp?, content_description?}`: `content` that
follows the finger sideways, as a deck's card does (decision `swipe-cards`).

Fields: content, on_swipe_left?, on_swipe_right?, threshold_dp?, content_description?.

A card to swipe (at least one side set): while dragged it moves with the finger, and past threshold_dp (default 96)
it tints toward the side it would go (right: success, left: error). Let go past it, or flung that way, it flies off
and runs that side's intent; let go before, it springs back. Put it in a core://List with stack: true to show one
card at a time.
