# Layouts are plumbing the app draws and porcelain the pod expands, defined as data

Date: 2026-11-05. Related: [layout-parts](layout-parts.md), [named-vocabulary](named-vocabulary.md),
[blueprints-and-home](blueprints-and-home.md), [layouts-not-apps](layouts-not-apps.md),
[pinned-app-title](pinned-app-title.md), [retired-layout-files](retired-layout-files.md), issue #124.

`core://Workspace` was drawn by the app: its title (fixed text, fixed style), its padding, the back arrow and 44 dp
inset of a pushed layout, the 16 dp spacer at the end, and the `fillsScreen` rule that dropped the title. Screens had
app-drawn chrome too (Back, title, spinner, error row with Try again, footer divider, scroll-to-end). A layout could
put nothing on the title line, had no data about where it was drawn, and every look change needed an app build. On
the pod, `core://Ref` already expanded blueprint parts before serving, but took no arguments, and nothing showed an
agent what a layout became. The blueprints repeated the same patterns (section headers, cards, pills, author headers,
like buttons) with hardcoded colours.

Decision: the vocabulary splits in two.

- **Plumbing** is what the app draws: the generic nodes, intents and device data, named features as before. It holds
  no opinion about page chrome. New plumbing:
  - `core://Page {id, title, root, screens: [{id, route, title?, data?, item?, root}]}`: any node as a layout's root,
    drawn over the whole space with no title, Back or insets from the app (feature `page`). `title` names it and is
    not drawn. Screens draw their own root. A pushed page draws its own Back.
  - `core://Scroll {content, follow_end?}` (feature `scroll`): scrolling as a node. In a bounded column (a page's
    root column) a scroll, a filling box or an inline layout takes the rest of the height; inside another scroll it
    doesn't scroll itself. `follow_end` follows the content to its end after an action from outside it (a footer).
  - `local://nav` (`can_go_back`, `presentation`, `pushed`, `inline`, `in_frame`) and, in a page screen,
    `local://screen` (`loading`, `error`) (feature `nav_data`). `action.navigation.back` and `action.data.load` do
    the rest.
  - `local://theme` (feature `theme`): the pod's theme colours, one light and one dark value per token, set with MCP
    `set_theme` (pod setting `theme`) and carried in every layout's bundle as the entity `core://theme`; the app
    picks the value for its own mode (it has only a light mode today). Per pod, not per layout, for now.
  - A page screen's `item` (feature `screen_item`): `$item` throughout the screen is the entity at that URI, instead
    of a one-row list. A list's `group_by.headers_only` (feature `list_group_headers`) draws one header per distinct
    value and no items, instead of zero-size spacers.
- **Porcelain** is defined as data in terms of plumbing and expanded by the pod before serving, so the app never sees
  it and changing it needs no app build. A **component** is `{doc, params: [{name, type, default?, required?, doc}],
  children?, template, examples: [{args, children?, note}]}`.
  - Parameters are values only: `text`, `number`, `bool`, `uri`, `binding`. The one exception is the caller's
    children, a single list of nodes.
  - In the template, `$arg.<name>` is an argument (a string that is only `"$arg.n"` takes the argument's own JSON
    type; it may also name an object key). `core://Children` is where the children go (`at: i` for one of them),
    `core://IfArg {arg, equals?, then, else?}` keeps a branch by an argument, and `core://Fragment {children}` stands
    for several siblings. Runtime bindings (`$item`, `$form`, `$route`, `${uri#f}`) pass through untouched.
  - A layout uses one with `{"type": "core://Use", "component", "args", "children", "version"?}`. `core://Ref {app,
    part}` stays as an alias of `core:<app>/<part>` (`part: "layout"`: `core:<app>`).
  - **Blueprints and components are one concept, called components, from two sources.** `core:` components ship with
    the server: files `crates/pod-core/components/<name>/NNNN.json` (`{note, component}`), recorded in each pod's
    `components` table at start and append-only like layout files (a changed file stops the start). Every blueprint is
    `core:<blueprint>` and its parts are `core:<blueprint>/<part>`; a component may not take a blueprint's name. `pod:`
    components are made by an agent on the pod: mutable, versioned (100 versions kept), restorable, visible to that
    pod only, at most 200. Deleting one that a layout's latest version uses is refused and names those layouts.
  - Expansion reuses the `core://Ref` seam: strict on a write (validation and recorded features describe the
    plumbing; every example of a definition must expand), lenient when serving (a missing component is "This part
    isn't available."), at most 8 uses deep and 1 MiB. Along the way it may build at most 4 MiB (approximate JSON:
    every node it visits and every copy of a use's children or arguments), checked as it goes, so a component that
    repeats its children or an argument can't multiply a use into gigabytes first. Each definition is read and parsed once per
    expansion and its template walked in place, so a use costs only what it builds, not a copy of its definition. Blueprint files may use `core:` components; the pod records
    the expanded layout, and the file's hash is of the file as written.
  - **Updates.** A layout whose latest version uses a component without `version` gets a `ref_update` version
    ("pod:card updated to v3") when the component changes: a `pod:` write, or a deploy with a new `core:` file.
  - **Features after expansion.** Serving re-checks each version's expansion against the client's features and steps
    back to an older version when it can't draw it. This closes the gap noted in `named-vocabulary`.
- **Workspace and Screen are porcelain.** `core:workspace` (args `title`, `title_scrolls`, `fills_screen`; children:
  the slots) and `core:screen` (args `title`, `data`, `has_footer`; children: content, then footer) expand to plumbing
  that reproduces the app's drawing. The pod serves a `core://Workspace` as that `core://Page` to an app that draws
  `page`, `scroll` and `nav_data`, and the workspace as stored to older apps and to callers that send no features.
  An agent copies `core:workspace` into `pod:` to change page chrome, or writes plumbing directly.
- **Agent tooling.** MCP `list_components`, `get_component` (with every example expanded), `expand` (a layout, a
  component use or any node, to plumbing, with its features), `define_component`, `update_component`,
  `copy_component` (records `copied_from`), `restore_component`, `delete_component`, `set_theme`; `get_layout` takes
  `expanded: true`, and `validate_layout` answers the expansion. `describe_vocabulary` is generated from the feature
  files (`crates/protocol/features/*.md`, which now carry each node's fields) and the component list, split into
  plumbing and porcelain.

Resolved questions from the issue: the syntax is `core://Use`, with `core://Ref` as an alias; components are node
trees only, and only whole blueprints carry screens; the theme is per pod, with a light and a dark value per token;
expansion stops at 8 uses deep, 1 MiB, or 4 MiB of work along the way, and a serve-time failure is the placeholder text as before.

Compatibility: only the new plumbing needs an app build, and existing feature negotiation handles it. Every refactor
was checked by expansion: the blueprint files rewritten onto components (`events` 0005, `settings` 0004, `inbox` 0002,
`shell` 0006, `recipes` 0006, `pixelfed` 0012) record exactly the layout of the file before them (golden test
`components_golden`), and the shell blueprints drawn as a workspace and as plumbing are pixel-identical (simulator test
`PlumbingGoldenTest`). The phase-0 cleanup: slot `provider_uri` is optional and ignored, the dead sticky inset and
end-inset local are gone, and the no-login sample (0004) defines the comments route its button opens.

Not done yet:

- Retiring the app's Workspace drawing (phase 6 of the issue): once every build in use draws `page`, move the client
  `workspace` node and screen chrome to `features/removed/` and delete their code.
- Migrating each pod's own layouts onto components and theme tokens (phase 5): an agent offers `expand`-checked
  rewrites per pod; nothing is rewritten automatically.
- The Pixelfed post screens are still three near-copies: a part can't take arguments, so one screen part with
  `focus?` needs a screen-level component, which this decision leaves out. Recipes' hero and stat tiles and the
  events view switch didn't match a component exactly and stay as written.
- Screens drawn through `core:screen` are close to the app's chrome but not pixel-identical (the bar's padding and
  the feed width limit on wide windows); root pages are identical.
