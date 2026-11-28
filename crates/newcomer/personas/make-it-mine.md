---
id: make-it-mine
tier: temporary
starts_from: groceries   # the same pod, a later visit
max_turns: 5
max_budget_usd: 5
---
## Who I am
The same person as groceries, back a few days later, wanting the app to feel like theirs.

## What I say
1. I miss the Settings button that was there at the start. Can you put it back, keeping my grocery list as the
   first thing I see?
2. And make the whole app dark, please, dark background and light text.

## If asked
- Colours: any dark theme is fine; not black and white only.
- Where Settings goes: anywhere easy to find on the first screen.

## Done when
sim: shot home; assert-text Groceries; click Settings; idle; wait-text 10000 Connected apps; shot settings
uses: @core/checklist
judge:
- The first screen still shows the grocery list.
- Settings can be opened from the first screen.
- The app is dark: a dark background with light text.
