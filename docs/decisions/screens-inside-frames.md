# Screens opened from a sheet or panel open inside it

Date: 2026-11-05. Related: [layouts-not-apps](layouts-not-apps.md), issue #119.

A screen opened from a presented layout (`action.navigation.open` with a `target_uri` matching one of its screens)
used to cover the whole screen with its own top bar, even when that layout was a Sheet or Panel: in Settings, a
layout's versions page or a notification-channel page left the drawer behind. Presenting a second layout as a Panel
instead was tried and rejected: it stacks a second drawer and dims the screen twice.

Decision: when the top of the back stack is a Sheet or Panel (or a screen already inside one), a screen of that
frame's layout opens inside the frame. The frame's content slides left as the screen slides in from the end, like a
push inside the drawer, with a Back arrow and the screen's title at its top; Back slides it out. The screens stay
ordinary back-stack entries, so system Back pops them first and then the frame, and a scrim tap or dragging a sheet
down closes the frame with everything inside it. It is generic for every Sheet and Panel: no new node, intent or
field, only client drawing (it needs a new desktop and Android build).

Screens of pushed layouts and of home, and a screen of another layout opened from inside a frame (home's, as the
fallback), still cover the whole screen.
