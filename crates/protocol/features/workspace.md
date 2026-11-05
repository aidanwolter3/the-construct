`core://Workspace`: the root of an app's layout, its slots stacked top to bottom.

Fields: id, title, slots: [{slot_id, provider_uri, content}], screens: [{id, route, title, data?, content, footer?}].

The root. Slots stack top to bottom; screens open on navigation (route like scheme://things/{id}; data is a data path loaded when it opens; $route.<param> in it).
