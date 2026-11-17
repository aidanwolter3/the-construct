`core://Layout` `panels: true`: inline, on an expanded screen (`size_class` `expanded`), a layout opened with
`action.navigation.open` (pushed) from inside it opens as a panel to its right instead of over the whole screen, and
one opened from a panel opens beside that panel. Back, or a back button in a panel, closes the rightmost panel; at most
two columns show (the embed and its panels), older ones moving out of sight to the left. Each panel binds the open's
`params` as `$route.<name>`. It doesn't gate a layout: on a narrower screen, and on a client without it, the open
pushes as before (decision `wide-screen-panels`).
