`core://Workspace`: the root of an app's layout, its slots stacked top to bottom.

Fields: id, title, slots: [{slot_id, provider_uri, content}], screens: [{id, route, title, data?, content, footer?}].

The root. Slots stack top to bottom; screens open on navigation (route like scheme://things/{id}; data is a data path loaded when it opens; $route.<param> in it).

Removed from clients (issue #126): a pod still accepts it and serves it to new builds as its expansion through
`core:workspace` / `core:screen` (`protocol::EXPANDED_FEATURES`).
