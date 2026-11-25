The intent `action.construct.swap {group, to}`: show an edit group's screens as they were (`to: "before"`) or as the
group made them (`to: "after"`) at once, from the elements the device already holds (the pod lists both in its
manifest's `swaps`), with no request (decision `instant-swap`). The pod is told separately (an Undo or Redo
mutation in the same `action.sequence`); its next root replaces the device's swap.
