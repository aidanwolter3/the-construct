---
id: groceries
tier: temporary          # temporary | fresh (needs the model or the web)
max_turns: 6             # user messages before it counts as stuck
max_budget_usd: 5
---
## Who I am
Shares a flat, shops twice a week, uses only a phone. Not technical.

## What I say
1. I want a grocery list on my phone called Groceries. I add things during the week and tick them off in the
   shop.
2. Can I see what's left to buy at the top, and the ticked things below?

## If asked
- Quantities: sometimes, written into the item ("2 oat milk").
- Shops: one supermarket, no aisles or categories.
- Where it goes: opening the app should get me to the list straight away.

## Done when
sim: shot home; click Groceries; idle; type title Oat milk; click tag:send:title; wait-text 10000 Oat milk
sim: shot added; click Oat milk; idle; shot ticked; assert-text Done
uses: @core/checklist
judge:
- Opening the app shows the grocery list, or reaches it in one tap.
- An item can be added and ticked off from the phone.
- Ticked items sit below the ones still to buy.
