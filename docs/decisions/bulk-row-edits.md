# Bulk row edits: confirm a kept preview by its token, and import rows from a file

Date: 2026-11-11. Related: [collections](collections.md), [collections-add-row](collections-add-row.md),
[web-and-agent-modules](web-and-agent-modules.md).

Importing 46 recipes into Collections (584 ingredient lines, 301 steps, 46 meal updates, 92 staples; about 215 KB)
took over 15 minutes, nearly all of it the agent writing arguments: `edit_rows` took every edit inline, the
confirming call needed the same edits again, and the agent split the set into batches of about 150 rows to keep each
call writable. The pod applied each batch almost at once. Considered: raising the batch size alone (still two
copies of everything, through the model), and a separate `import_rows` tool (a second preview and confirm flow to
keep in step with the first).

Decision: one tool, `edit_rows`, with three ways in and the same preview and confirm token:

- **The pod keeps the preview.** A preview stores its edits under its confirm token (`collections_staged`, an hour,
  the newest five per pod). `edit_rows {collection, confirm}` with no edits applies them: the token stays a hash of
  the edits, the definition version and the affected rows' state, so if anything changed meanwhile the call answers
  `stale_preview` with a new preview (kept too) and changes nothing. Applying forgets the preview, so a token
  applies once. Sending the edits again with the token still works.
- **Edits from a file.** `edit_rows {collection, url, format?}` fetches a file through the web module (host policy,
  public hosts; needs the `web` jack), and `POST /v1/collections/<c>/import?format=&note=` takes the file as the body
  from a client or agent holding the pod token (up to 8 MiB). Formats: a JSON array of edits or of `{field: value}`
  rows, JSON lines of the same, or CSV with a header of field names, optional `op` and `id` columns, and
  `<ref field>.<unique field>` columns naming a ref's row (`recipe.name`). Both only preview; applying is
  `edit_rows {collection, confirm}` over MCP, where Claude shows the user the preview first, so the token stays the
  user's approval.
- **The whole set at once.** A preview checks every edit (values, refs, filler ownership, then the whole set
  written and rolled back for the caps and unique fields, so duplicates within the file are found before
  applying) and refuses with every invalid edit and where it is (`/edits/<i>`, `line <n>`, `item <n>`; the first 50
  listed, all counted). One edit may touch as many rows as a pod holds (5,000, was 500), and the tool tells the agent
  to send the whole set in one call.

Not done: refs to rows created in the same call (the parents go first, in their own edit); clearing a field from a
CSV (an empty cell leaves the field out; use JSON with `null`); the import route takes one file per request, not a
multipart upload.
