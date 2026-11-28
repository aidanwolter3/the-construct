---
id: reading
tier: temporary
max_turns: 6
max_budget_usd: 5
---
## Who I am
Reads on the train, keeps losing track of recommendations. Uses only a phone. Not technical.

## What I say
1. I want to keep track of books: the ones I want to read, and the ones I've finished with a rating out of 5.
   Put a Books button on my home screen.
2. Add Piranesi and The Overstory to read. I finished Klara and the Sun and give it 4.

## If asked
- Ratings: whole numbers from 1 to 5, only for finished books.
- Authors: nice to have, not needed.
- Layout: one screen is fine, with the to-read books first.

## Done when
sim: shot home; click Books; idle; wait-text 10000 Piranesi; shot books; assert-text Klara
judge:
- Home reaches the books in one tap.
- Books still to read and finished books are clearly apart.
- The finished book shows its rating of 4.
- There is a way to add a book from the phone.
