# Claude edits a recipe's notes with the MCP tool `recipes_notes`

Date: 2026-11-16. Related: D41, [recipes-start-empty](recipes-start-empty.md).

Recipes have always had notes (`recipes_notes`, D41), and the recipe screen shows them under "Notes", but nothing
could write them: the app is read-only and there was no import path. The user wants to keep what they learn while
cooking (for example a cook time at high altitude) on the recipe itself.

Considered: an action endpoint and a text field on the recipe screen (needs a layout change and an input the user
didn't ask for), or moving recipes into a collection (a much larger change).

Decision: the recipes source offers one MCP tool, `recipes_notes {op: list | add | update | remove, recipe, n?,
text?}`. `recipe` is a recipe's slug, its title (any case) or part of one title (part of several answers
`ambiguous_recipe` with the matches, so Claude can pick one's slug); notes are numbered from 1 in screen order. A write
replaces the recipe's notes in one transaction (positions renumbered from 0, so a removed note leaves no gap) and
publishes the recipes source's data, so open recipe screens refresh. A note is at most 2000 characters.

Consequences: no schema change, no layout change and no client build. Not done: editing notes from the app, and
editing a recipe's other parts (ingredients, steps) from Claude.
