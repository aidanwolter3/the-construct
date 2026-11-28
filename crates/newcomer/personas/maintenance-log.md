---
id: maintenance-log
tier: temporary
max_turns: 6
max_budget_usd: 5
---
## Who I am
Owns a house and a car, forgets when things were last done. Uses only a phone. Not technical.

## What I say
1. I keep forgetting when I last did things around the house and the car. Can you give me a Maintenance log on
   my home screen where I write down what I did and when, newest first?
2. Put these in: I serviced the Honda on 2026-10-15, and the Bosch boiler had its yearly check on 2026-11-02.
3. On the home screen, show me the last thing I did, so I see it without opening the log.

## If asked
- What to record: what was done, the date, and sometimes a note (a mileage, who did it).
- Categories: not needed.
- Reminders: not now.

## Done when
sim: shot home; assert-text Bosch; click Maintenance; idle; wait-text 10000 Honda; shot log
judge:
- Home shows the most recent thing done (the Bosch boiler check) without opening the log.
- The log lists both entries with their dates, newest first.
- There is a way to add a new entry from the phone.
