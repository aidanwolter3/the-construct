# One row of a public link has its own address, named by the row's name

Date: 2026-11-18. Related: [short-paths-without-at](short-paths-without-at.md),
[public-links-share-the-closure](public-links-share-the-closure.md).

A shared recipe book is one public link (`/aidan/recipes`), and tapping a recipe pushes its screen from the copy. The
owner wants to send one recipe without sharing it separately, and a name in the address rather than an id.

Decision: `/<handle>/<slug>/<row>` answers the page, and the page opens that row's screen over the list, as a tap on
the row would; Back (in the page or the browser) shows the list. As a visitor opens a row from the list, the address
bar shows that row's address, which is how the owner gets the link: open the recipe, copy the address.

- **Which rows:** those of the first list in the copy's root whose rows open a screen (`action.navigation.open`).
  A list whose rows open a web page (events) has none. Nothing is set per row: every row the copy carries has an
  address, new rows included.
- **Names:** the row's `name` (or `title`) as lowercase words joined by `-` ("Apple (Finnish) Pancakes" is
  `apple-finnish-pancakes`); a row without one is named by its `id`. Equal names, in the list's order: the first keeps
  it, the next are `-2`, `-3`, …; `view` and `image` (the page's own paths) are `-2` too.
- **Renames:** the address follows the name. An old name no row has opens the list (a 200, not a 404), so a link to a
  renamed recipe still lands on the book. Keeping old names would need the names stored per row; not done.
- **One reader:** `protocol::public_rows` reads the copy for both the host (the page's title is the row's, for link
  previews) and the web client (which row to open, and the address of an open row's screen).
- Only short paths: `/s/<link>/<row>` isn't served.
