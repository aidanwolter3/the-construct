`action.confirm {title, message?, confirm_label, then}` asks "Are you sure?" before a step that can't be undone: the
app shows its own dialog with the layout's wording and runs `then` (any intent) only when the user confirms. In an
`action.sequence`, the steps after it wait for the answer too.
