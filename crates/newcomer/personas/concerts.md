---
id: concerts
tier: fresh              # needs the web and model jacks, locked on temporary pods
max_turns: 6
max_budget_usd: 8
---
## Who I am
Likes live music, lives in a mid-sized city, never hears about gigs in time. Uses only a phone.

## What I say
1. Can you tell me about concerts near me this month? I'd like to see them on my phone and get a reminder the
   day before one I pick.

## If asked
- City: Leeds.
- Music: indie and folk.
- Venues: any.

## Done when
sim: shot home; click Concerts; idle; shot concerts
judge:
- Home reaches a list of upcoming concerts.
- The concerts listed are in Leeds and this month.
- A concert can be picked for a reminder.
