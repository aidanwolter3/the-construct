`bind` on a `core://Layout` or `action.navigation.open` of the content-addressed construct: the use site binds the
target layout's inputs (`$in.<name>`), home from the pod's data, every other layout from its own inputs (decision
`content-addressed-construct`). The engine fills them in when it materializes the target and loads only the data
they bind.
