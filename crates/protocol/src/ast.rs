//! Namespaced UI AST (docs/layouts.md).
//!
//! Every node is generic (`core://`): apps are composed from these on the pod
//! (docs/DECISIONS.md D34-D36); no client knows any app. On the wire each node
//! is a JSON object whose `"type"` field selects the variant. A node of a type
//! this build does not know (e.g. the removed `pixelfed://` widgets in old
//! layout versions) deserializes as [`UiNode::Unknown`] holding its JSON, and
//! serializes back unchanged, so a pod can still serve old versions to the
//! clients that draw them.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::intent::{ActionIntent, Animation, Edge};

/// Node types this build knows (an other type is [`UiNode::Unknown`]).
pub const NODE_TYPES: [&str; 30] = [
    "core://Workspace",
    "core://Page",
    "core://Scroll",
    "core://Container",
    "core://Text",
    "core://Button",
    "core://List",
    "core://Image",
    "core://TextField",
    "core://Row",
    "core://Column",
    "core://Spacer",
    "core://Divider",
    "core://Conditional",
    "core://Carousel",
    "core://IconButton",
    "core://Chip",
    "core://Badge",
    "core://Tappable",
    "core://Box",
    "core://ChoiceChips",
    "core://MediaPicker",
    "core://Sticky",
    "core://Progress",
    "core://Dock",
    "core://AppFrame",
    "core://Switch",
    "core://Drawer",
    "core://Layout",
    "core://Gesture",
];

/// Icon names clients draw (`core://IconButton`, `core://Chip`).
pub const ICONS: [&str; 23] = [
    "favorite",
    "favorite_border",
    "chat_bubble",
    "bookmark",
    "bookmark_border",
    "repeat",
    "warning",
    "send",
    "arrow_back",
    "close",
    // Schema 6 (D44): older clients draw a chip with these without its icon.
    "flame",
    "timer",
    // Schema 9 (D51).
    "add_photo",
    "add",
    "refresh",
    // Feature `todo_icons` (crates/protocol/features/todo_icons.md).
    "check",
    "check_box",
    "check_box_outline_blank",
    "delete",
    // Feature `icons_shell` (crates/protocol/features/icons_shell.md).
    "settings",
    "notifications",
    // Feature `icons_cart` (crates/protocol/features/icons_cart.md).
    "shopping_cart",
    // Feature `icons_stapler` (crates/protocol/features/icons_stapler.md).
    "stapler",
];

fn one() -> f64 {
    1.0
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn is_zero(v: &u16) -> bool {
    *v == 0
}

fn is_square(s: &ImageShape) -> bool {
    *s == ImageShape::Square
}

/// A single node in the server-driven UI tree.
// `IconButton` (an inline intent) dwarfs other variants; layout trees are
// small and built rarely, so the size cost is irrelevant.
#[allow(clippy::large_enum_variant)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", remote = "Self")]
pub enum UiNode {
    // Core layout primitives
    #[serde(rename = "core://Workspace")]
    Workspace {
        id: String,
        title: String,
        slots: Vec<SlotSpec>,
        /// Screens this app can open (docs/DECISIONS.md D34): an
        /// `action.navigation.open` whose `target_uri` matches a screen's
        /// `route` pushes that screen. Omitted when empty; clients that predate
        /// screens ignore it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        screens: Vec<Screen>,
        /// The title scrolls away with the content (feature
        /// `title_scrolls`, decision `pinned-app-title`). By default
        /// clients pin it in a bar above the scrolling content.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        title_scrolls: bool,
    },
    /// A layout's root in plumbing (feature `page`, decision
    /// `plumbing-and-porcelain`): any node as `root`, drawn filling the
    /// space the layout gets, with no chrome from the app (no title, back
    /// arrow or insets). `title` names the layout (lists, accessibility)
    /// and is not drawn. `screens` open by route as a workspace's do; each
    /// draws its own `root`.
    #[serde(rename = "core://Page")]
    Page {
        id: String,
        #[serde(default)]
        title: String,
        root: Box<UiNode>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        screens: Vec<PageScreen>,
    },
    /// `content` scrolling vertically (feature `scroll`), filling the
    /// height its parent leaves (in a `core://Column` with a bounded height,
    /// the rest of it). `core://Sticky` inside pins at its top. With
    /// `follow_end`, after any action of the page it follows its content to
    /// the end once the content grows (a new comment). Inside another
    /// scrolling view it doesn't scroll itself.
    #[serde(rename = "core://Scroll")]
    Scroll {
        content: Box<UiNode>,
        #[serde(default, skip_serializing_if = "is_false")]
        follow_end: bool,
    },
    #[serde(rename = "core://Container")]
    Container {
        orientation: Orientation,
        padding_dp: u16,
        children: Vec<UiNode>,
    },
    /// `value_expr`: `$item.<field>` (inside a list or carousel template), text
    /// with embedded `$item.<field>` tokens, `${<uri>#<field>}` placeholders,
    /// or plain text. `format` (feature `date_format`) draws the dates it
    /// binds for the viewer ([`TextFormat`]); `count` (feature `count`)
    /// puts a count of a query's entities in place of `$count`
    /// ([`TextCount`]).
    #[serde(rename = "core://Text")]
    Text {
        value_expr: String,
        style: TextStyle,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<TextFormat>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        count: Option<TextCount>,
    },
    #[serde(rename = "core://Button")]
    Button {
        label: String,
        on_click: ActionIntent,
    },

    /// A list of the entities of `queries[query_key]`, each rendered with
    /// `item_template` (`$item.<field>` binds the entity). `indent` shifts an
    /// item right by `item[indent.field] * step_dp` (capped at `max` steps).
    /// `empty_text` shows when the query is present but empty. `filter` and
    /// `group_by` are schema 5 (docs/DECISIONS.md D43): see [`ListFilter`]
    /// and [`GroupBy`]; `where` is schema 6 (D44, [`ListWhere`]).
    #[serde(rename = "core://List")]
    List {
        query_key: String,
        item_template: Box<UiNode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        indent: Option<Indent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        empty_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<ListFilter>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group_by: Option<GroupBy>,
        /// Schema 6 (D44): see [`ListWhere`]; a list of conditions that all
        /// match is feature `list_where_all` ([`WhereClause`]).
        #[serde(rename = "where", default, skip_serializing_if = "Option::is_none")]
        where_: Option<WhereClause>,
        /// Schema 8 (D48): items side by side in this many equal columns
        /// (a grid); 0 and 1 mean one item per row. Group headers span the
        /// width and start a new row; `indent` applies to one column only.
        #[serde(default, skip_serializing_if = "is_zero")]
        columns: u16,
        /// Schema 8 (D48): space between items (both ways in a grid).
        #[serde(default, skip_serializing_if = "is_zero")]
        spacing_dp: u16,
        /// Schema 8 (D48): space left and right of the items.
        #[serde(default, skip_serializing_if = "is_zero")]
        padding_h_dp: u16,
        /// Schema 12: one item to scroll to and tint ([`ListFocus`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        focus: Option<ListFocus>,
    },
    /// An image from a URL (a literal or a binding such as
    /// `"$item.avatar_url"`). `size_dp` makes it a square of that size;
    /// otherwise it fills the width at `aspect_ratio` (default 1). `blurhash`
    /// (a binding or literal) colours the placeholder; when the image fails,
    /// `content_description` is shown in its place.
    #[serde(rename = "core://Image")]
    Image {
        url: String,
        #[serde(default, skip_serializing_if = "is_square")]
        shape: ImageShape,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size_dp: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        aspect_ratio: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blurhash: Option<String>,
    },
    /// A text input whose value is the form value `$form.<field>` for the
    /// intents of its screen. `on_submit` (a send button, and the keyboard's
    /// submit) is dispatched with the value bound, then the field is cleared;
    /// it is not dispatched while the field is blank. `lines` (schema 9,
    /// D51) makes a field without `on_submit` multi-line, growing up to that
    /// many lines (a caption).
    #[serde(rename = "core://TextField")]
    TextField {
        field: String,
        #[serde(default)]
        placeholder: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_length: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_submit: Option<ActionIntent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines: Option<u32>,
    },

    // Schema 3 (docs/DECISIONS.md D36)
    /// Children side by side, vertically centred, never wrapping: a
    /// `core://Spacer` without a size takes the remaining width.
    #[serde(rename = "core://Row")]
    Row {
        children: Vec<UiNode>,
        #[serde(default, skip_serializing_if = "is_zero")]
        spacing_dp: u16,
        #[serde(default, skip_serializing_if = "is_zero")]
        padding_h_dp: u16,
        #[serde(default, skip_serializing_if = "is_zero")]
        padding_v_dp: u16,
        /// Schema 6 (D44): `Start` puts the children at the top, `End` at the
        /// bottom (default: centred).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<Align>,
    },
    /// Children stacked, full width. `align` (schema 6, D44) places children
    /// narrower than the column (default `Start`).
    #[serde(rename = "core://Column")]
    Column {
        children: Vec<UiNode>,
        #[serde(default, skip_serializing_if = "is_zero")]
        spacing_dp: u16,
        #[serde(default, skip_serializing_if = "is_zero")]
        padding_h_dp: u16,
        #[serde(default, skip_serializing_if = "is_zero")]
        padding_v_dp: u16,
        #[serde(default, skip_serializing_if = "is_start")]
        align: Align,
    },
    /// Empty space: `size_dp` along the parent's axis, or (no size, in a
    /// `core://Row`) all the remaining width.
    #[serde(rename = "core://Spacer")]
    Spacer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size_dp: Option<u16>,
    },
    /// A thin horizontal line.
    #[serde(rename = "core://Divider")]
    Divider {},
    /// `content` only when `value` (a binding such as `$item.caption`) is
    /// truthy: not missing, null, false, 0, "", "0" or an empty array. Schema
    /// 8 (D48): `value` may be a form value (`$form.<field>`, "" while unset),
    /// and with `equals` the content shows only when the value, as text,
    /// equals it (`"equals": ""` shows it while the value is unset or empty).
    #[serde(rename = "core://Conditional")]
    Conditional {
        value: String,
        content: Box<UiNode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        equals: Option<String>,
    },
    /// A swipeable pager over entities: `items` is a binding to a list of
    /// entity URIs (`"$item.media_uris"`) or a literal list; each is drawn with
    /// `item_template` (`$item` = that entity). Page dots and an "n/N" badge
    /// show with more than one item. The aspect ratio is the first item's
    /// `aspect_field` (clamped to `min_aspect..=max_aspect`), else
    /// `aspect_ratio`. `tag` (bindable) names it for tests and accessibility.
    #[serde(rename = "core://Carousel")]
    Carousel {
        items: ListBinding,
        item_template: Box<UiNode>,
        #[serde(default = "one")]
        aspect_ratio: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        aspect_field: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_aspect: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_aspect: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
    },
    /// An icon button. `active` (a binding to a bool, e.g. `$item.is_liked`)
    /// switches to `active_icon`, `active_tint` (`#RRGGBB`) and
    /// `active_content_description`. Icons are named ([`ICONS`]).
    #[serde(rename = "core://IconButton")]
    IconButton {
        icon: String,
        content_description: String,
        on_click: ActionIntent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_icon: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_tint: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_content_description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size_dp: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
    },
    /// A small label with an optional icon, coloured by `tone`.
    #[serde(rename = "core://Chip")]
    Chip {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        #[serde(default)]
        tone: Tone,
    },
    /// A one-line note with a small icon, e.g. "Bob boosted".
    #[serde(rename = "core://Badge")]
    Badge { text: String, icon: String },

    // Schema 4 (docs/DECISIONS.md D42)
    /// `content` (any node) made tappable: a tap anywhere on it dispatches
    /// `on_click` (with `$item` bound inside a list). A button inside keeps its
    /// own tap. `content_description` is read by screen readers; `tag`
    /// (bindable) names it for tests.
    #[serde(rename = "core://Tappable")]
    Tappable {
        on_click: ActionIntent,
        content: Box<UiNode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
    },

    // Schema 6 (docs/DECISIONS.md D44)
    /// `content` on a coloured, rounded surface. `background`, `content_color`
    /// (text and icons inside) and `border_color` are `#RRGGBB`, or a binding
    /// that resolves to one (`$item.color`, `${<uri>#color}`); one that does
    /// not resolve draws nothing. Without `width_dp` / `height_dp` the box
    /// wraps its content (a `core://Column` inside fills the width); with
    /// them, `align` places the content inside it.
    #[serde(rename = "core://Box")]
    Box {
        content: Box<UiNode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_color: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        border_color: Option<String>,
        #[serde(default, skip_serializing_if = "is_zero")]
        corner_dp: u16,
        #[serde(default, skip_serializing_if = "is_zero")]
        padding_h_dp: u16,
        #[serde(default, skip_serializing_if = "is_zero")]
        padding_v_dp: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width_dp: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height_dp: Option<u16>,
        #[serde(default, skip_serializing_if = "is_start")]
        align: Align,
        /// Feature `box_fill`: fill the space given, with `content` placed by
        /// `align` in both directions (`Center`: the middle). As a
        /// workspace's one slot it fills the whole screen, with no title bar.
        #[serde(default, skip_serializing_if = "is_false")]
        fill: bool,
    },
    /// A one-line, sideways-scrolling row of chips editing the form value
    /// `$form.<field>` (like a `core://TextField`): tapping an option sets
    /// the value to the option's `value` (an empty value clears it). The
    /// option whose value equals the form value (or the empty one, while the
    /// value is unset) is drawn selected.
    ///
    /// Schema 10: with `remember`, the client keeps the chosen value on the
    /// device (per app and screen) and restores it the next time the layout
    /// or screen opens, e.g. a List/Cards switch. Only a value that is still
    /// one of the options is restored.
    #[serde(rename = "core://ChoiceChips")]
    ChoiceChips {
        field: String,
        options: Vec<ChoiceOption>,
        #[serde(default, skip_serializing_if = "is_zero")]
        padding_h_dp: u16,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        remember: bool,
    },

    // Schema 9 (docs/DECISIONS.md D49, D51)
    /// Pick photos from the device: thumbnails of the picked ones (each with
    /// a remove button) and an add tile while fewer than `max_items` are
    /// picked. The form value `$form.<field>` is the list of staged
    /// attachment ids, for an `action.mutation.create`'s `attachments`.
    #[serde(rename = "core://MediaPicker")]
    MediaPicker {
        field: String,
        max_items: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },

    // Schema 11 (docs/decisions/sticky-nodes.md)
    /// `content` that scrolls with the page until it reaches the top of the
    /// scrolling view, then stays pinned there while the rest of its parent
    /// scrolls under it (like CSS `position: sticky`). It scrolls away with
    /// the end of its parent. Pinned, it is drawn on the page background, so
    /// what scrolls under it is hidden. Only meaningful in a scrolling
    /// column: not inside a `core://List` or `core://Carousel` template or
    /// another `core://Sticky` (rejected by the pod's validation).
    #[serde(rename = "core://Sticky")]
    Sticky { content: Box<UiNode> },

    // Feature `progress` (crates/protocol/features/progress.md)
    /// An indeterminate circular spinner for work in progress, `size_dp`
    /// across (default 16), drawn in a quiet theme colour. It has no
    /// children and no intents; show it while a status field says so with a
    /// `core://Conditional` around it.
    #[serde(rename = "core://Progress")]
    Progress {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size_dp: Option<u16>,
    },

    // Feature `dock` (crates/protocol/features/dock.md)
    /// The app's main navigation: a bar of tabs along the bottom (a rail
    /// down the side on wide windows) and, above it, the selected tab's
    /// `content`, filling the rest of the screen. The tabs are `before`,
    /// then one per entity of `query_key` drawn from `item` (`$item` = that
    /// entity; at most `max_items` of them), then `after`. The selected
    /// tab's `value` is the form value `$form.<field>` (the first tab until
    /// one is picked), kept on the device. Only meaningful as the content
    /// of a workspace's one slot (decision `blueprints-and-home`).
    #[serde(rename = "core://Dock")]
    Dock {
        field: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        before: Vec<DockTab>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query_key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<Box<DockTab>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_items: Option<u8>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        after: Vec<DockTab>,
    },

    // Feature `app_frame` (crates/protocol/features/app_frame.md)
    /// Another app of the pod, drawn here whole: its layout, its screens
    /// and its Back stack, filling the space it is given. `app` is an app
    /// id or a binding to one (`$item.id`). A frame whose app can't be drawn
    /// says so in its own space; the rest of the layout keeps working.
    #[serde(rename = "core://AppFrame")]
    AppFrame { app: String },

    // Feature `drawer` (crates/protocol/features/drawer.md)
    /// The app's main navigation as a side drawer: the selected page's
    /// `content` fills the screen, and a menu button at the top left (with
    /// `badge`, a binding drawn as a count or a dot) opens `drawer`, a panel
    /// of any layout. Pages are `before`, one per entity of `query_key`
    /// drawn from `item` (`$item` = that entity), then `after`; the selected
    /// page's `value` is the form value `$form.<field>` (the first page until
    /// one is picked), kept on the device. `action.device` `open_app` or
    /// `show` picks a page; any action from inside the panel closes it. Only
    /// meaningful as the content of a workspace's one slot (decision
    /// `blueprints-and-home`).
    #[serde(rename = "core://Drawer")]
    Drawer {
        field: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        before: Vec<DrawerPage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query_key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<Box<DrawerPage>>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        after: Vec<DrawerPage>,
        drawer: Box<UiNode>,
        /// Pinned to the bottom of the panel while `drawer` scrolls above it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        footer: Option<Box<UiNode>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        badge: Option<String>,
    },

    // Feature `switch` (crates/protocol/features/switch.md)
    /// An on/off switch: on while `checked` (a binding such as
    /// `$item.enabled`) is truthy; a tap dispatches `on_click`, which changes
    /// what `checked` reads (e.g. `action.device` `toggle_channel`).
    #[serde(rename = "core://Switch")]
    Switch {
        checked: String,
        on_click: ActionIntent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_description: Option<String>,
    },

    // Feature `layout_embed` (crates/protocol/features/layout_embed.md)
    /// Another layout of the pod drawn here (decision `layouts-not-apps`).
    /// `Inline` (the default) draws it in place, `height_dp` tall when set;
    /// `Sheet` and `Panel` present it over the screen, and so does `Overlay`
    /// from its `edge` (feature `overlay_edge`). With `show_when` (a
    /// binding such as `$form.menu` or `${local://inbox#unread}`) it shows
    /// only while that is truthy, or equals `equals`; dismissing a presented
    /// one clears the `$form` field `show_when` names. `params` bind as
    /// `$route.<name>` in the embedded layout.
    #[serde(rename = "core://Layout")]
    Layout {
        layout: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        show_when: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        equals: Option<String>,
        #[serde(default, skip_serializing_if = "EmbedPresentation::is_inline")]
        presentation: EmbedPresentation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        animation: Option<Animation>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        params: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height_dp: Option<u16>,
        /// With presentation `Overlay`: the edge it comes in from (bottom
        /// when unset).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        edge: Option<Edge>,
        /// With presentation `Overlay`: its height (top, bottom) or width
        /// (start, end).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size_dp: Option<u16>,
    },

    // Feature `gesture` (crates/protocol/features/gesture.md)
    /// `content` with gesture handlers (decision
    /// `edge-overlays-and-gestures`), each an ordinary intent; at least one
    /// is set. A swipe is a drag mostly in one direction; a pull the content
    /// can't scroll for counts, so a page inside still scrolls and pulling
    /// it down at its top is a swipe down. `core://Tappable` is the tap case.
    #[serde(rename = "core://Gesture")]
    Gesture {
        content: Box<UiNode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_tap: Option<ActionIntent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_long_press: Option<ActionIntent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_swipe_up: Option<ActionIntent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_swipe_down: Option<ActionIntent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_swipe_left: Option<ActionIntent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_swipe_right: Option<ActionIntent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_description: Option<String>,
    },

    /// A node of a type this build does not know: its JSON, kept as is.
    #[serde(skip)]
    Unknown(Value),
}

impl Serialize for UiNode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            UiNode::Unknown(raw) => raw.serialize(s),
            // The derived (remote = "Self") serializer.
            known => UiNode::serialize(known, s),
        }
    }
}

impl<'de> Deserialize<'de> for UiNode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(d)?;
        let known = raw
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| NODE_TYPES.contains(&t));
        if !known {
            return Ok(UiNode::Unknown(raw));
        }
        // The derived (remote = "Self") deserializer: a known type must be valid.
        UiNode::deserialize(raw).map_err(D::Error::custom)
    }
}

impl UiNode {
    /// The node's `"type"` tag (an unknown node's own, or "").
    pub fn type_tag(&self) -> String {
        match self {
            UiNode::Unknown(raw) => raw
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            known => serde_json::to_value(known)
                .ok()
                .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default(),
        }
    }

    /// Direct child nodes (screens' content and footers included).
    pub fn children(&self) -> Vec<&UiNode> {
        match self {
            UiNode::Workspace { slots, screens, .. } => slots
                .iter()
                .map(|s| &s.content)
                .chain(
                    screens
                        .iter()
                        .flat_map(|s| std::iter::once(&s.content).chain(s.footer.as_ref())),
                )
                .collect(),
            UiNode::Page { root, screens, .. } => std::iter::once(root.as_ref())
                .chain(screens.iter().map(|s| &s.root))
                .collect(),
            UiNode::Container { children, .. }
            | UiNode::Row { children, .. }
            | UiNode::Column { children, .. } => children.iter().collect(),
            UiNode::List {
                item_template,
                group_by,
                ..
            } => std::iter::once(item_template.as_ref())
                .chain(group_by.as_ref().map(|g| g.header.as_ref()))
                .collect(),
            UiNode::Carousel { item_template, .. } => vec![item_template],
            UiNode::Conditional { content, .. }
            | UiNode::Tappable { content, .. }
            | UiNode::Gesture { content, .. }
            | UiNode::Box { content, .. }
            | UiNode::Sticky { content }
            | UiNode::Scroll { content, .. } => {
                vec![content]
            }
            UiNode::Dock {
                before,
                item,
                after,
                ..
            } => before
                .iter()
                .chain(item.as_deref())
                .chain(after)
                .map(|t| t.content.as_ref())
                .collect(),
            UiNode::Drawer {
                before,
                item,
                after,
                drawer,
                footer,
                ..
            } => before
                .iter()
                .chain(item.as_deref())
                .chain(after)
                .map(|p| &p.content)
                .chain(std::iter::once(drawer.as_ref()))
                .chain(footer.as_deref())
                .collect(),
            UiNode::Text { .. }
            | UiNode::AppFrame { .. }
            | UiNode::Layout { .. }
            | UiNode::Switch { .. }
            | UiNode::Button { .. }
            | UiNode::Image { .. }
            | UiNode::TextField { .. }
            | UiNode::Spacer { .. }
            | UiNode::Divider {}
            | UiNode::IconButton { .. }
            | UiNode::Chip { .. }
            | UiNode::Badge { .. }
            | UiNode::ChoiceChips { .. }
            | UiNode::MediaPicker { .. }
            | UiNode::Progress { .. }
            | UiNode::Unknown(_) => Vec::new(),
        }
    }

    /// The intents this node itself holds (not its children's).
    pub fn own_intents(&self) -> Vec<&ActionIntent> {
        let direct: Vec<&ActionIntent> = match self {
            UiNode::Button { on_click, .. }
            | UiNode::IconButton { on_click, .. }
            | UiNode::Tappable { on_click, .. }
            | UiNode::Switch { on_click, .. } => vec![on_click],
            UiNode::TextField { on_submit, .. } => on_submit.iter().collect(),
            UiNode::Gesture { .. } => self.gesture_handlers().into_iter().flatten().collect(),
            _ => Vec::new(),
        };
        // The steps of an `action.sequence` count as the node's intents too,
        // so features, schema checks and the pod's validation see them.
        direct.into_iter().flat_map(ActionIntent::flatten).collect()
    }

    /// A `core://Gesture`'s handlers: tap, long press, swipe up, down, left,
    /// right (empty for any other node).
    pub fn gesture_handlers(&self) -> Vec<Option<&ActionIntent>> {
        match self {
            UiNode::Gesture {
                on_tap,
                on_long_press,
                on_swipe_up,
                on_swipe_down,
                on_swipe_left,
                on_swipe_right,
                ..
            } => [
                on_tap,
                on_long_press,
                on_swipe_up,
                on_swipe_down,
                on_swipe_left,
                on_swipe_right,
            ]
            .into_iter()
            .map(Option::as_ref)
            .collect(),
            _ => Vec::new(),
        }
    }

    /// Every node of the tree, depth first (this one first).
    pub fn walk(&self) -> Vec<&UiNode> {
        let mut out = vec![self];
        let mut i = 0;
        while i < out.len() {
            let children = out[i].children();
            out.extend(children);
            i += 1;
        }
        out
    }
}

/// A screen an app can open (docs/DECISIONS.md D34). `route` is a URI
/// pattern whose `{name}` segments capture route parameters (each
/// `[A-Za-z0-9_-]{1,64}`), e.g. `pixelfed://posts/{id}/comments`. Every
/// `$route.<name>` in `title`, `data`, `content` and `footer` is replaced by
/// the captured value. `data`, when set, is a pod path loaded when the screen
/// opens (`action.data.load`); its answer is a [`crate::DataBundle`].
/// `footer` stays at the bottom while `content` scrolls.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Screen {
    pub id: String,
    pub route: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    pub content: UiNode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footer: Option<UiNode>,
}

/// A screen of a `core://Page` (feature `page`): opened by `route` as a
/// workspace's [`Screen`] is, with `$route.<name>` substituted everywhere in
/// it, and `data` loaded when it opens. It draws `root` filling the screen,
/// with no chrome from the app. `title` names it (not drawn). `item`
/// (feature `screen_item`) binds `$item` throughout the screen to the entity
/// at that URI, e.g. `pixelfed://posts/$route.id`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PageScreen {
    pub id: String,
    pub route: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    pub root: UiNode,
}

/// `core://List` indentation: `item[field]` (a number) times `step_dp`,
/// capped at `max` steps (default 4).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Indent {
    pub field: String,
    pub step_dp: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u16>,
}

/// `core://List` filtering (schema 5, D43), done by the client as you type.
/// `value` is the query: `$form.<field>` (the form value a `core://TextField`
/// with that `field` edits) or literal text. While it is blank every item is
/// drawn. Otherwise an item is drawn only when every word of the query (split
/// on whitespace) is found in at least one of the item's `fields`; matching
/// ignores case and accents ("creme" finds "Crème"). `no_match_text` shows
/// when the query hides every item (default: the list's `empty_text`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ListFilter {
    pub value: String,
    pub fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_match_text: Option<String>,
}

/// `core://List` group headers (schema 5, D43): before each drawn item whose
/// `item[field]` differs from the previous drawn item's, the list draws
/// `header` with `$item` bound to that item. Headers follow the drawn items,
/// so they stay right while a [`ListFilter`] hides some. The query should
/// already be ordered by `field`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct GroupBy {
    pub field: String,
    pub header: Box<UiNode>,
    /// Feature `list_group_headers`: draw only the headers, one per run of
    /// equal values (the distinct values), and not the items.
    #[serde(default, skip_serializing_if = "is_false")]
    pub headers_only: bool,
}

/// `core://List` `where` (schema 6, D44): while `equals` (`$form.<field>` or
/// literal text) is non-blank, only items whose `field` equals it exactly are
/// drawn. Applied before the list's [`ListFilter`]; both can be set (a
/// category picker and a search box over one list).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ListWhere {
    pub field: String,
    pub equals: String,
}

/// A `core://List` `where`: one [`ListWhere`] (`{field, equals}`), or
/// (feature `list_where_all`, decision `list-where-all`) a list of them that
/// must all match, the same shape as a `core://Text` [`TextCount`]'s `where`.
/// Each condition is applied as a single one is (a blank `equals` matches
/// everything). Untagged, so a layout is served in the form it was written.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum WhereClause {
    One(ListWhere),
    All(Vec<ListWhere>),
}

impl WhereClause {
    /// Every condition, in order.
    pub fn conditions(&self) -> &[ListWhere] {
        match self {
            WhereClause::One(w) => std::slice::from_ref(w),
            WhereClause::All(all) => all,
        }
    }
}

impl From<ListWhere> for WhereClause {
    fn from(w: ListWhere) -> Self {
        WhereClause::One(w)
    }
}

/// `core://List` `focus` (schema 12, docs/decisions/list-focus.md): the item
/// whose entity URI equals `uri` (usually built from a route parameter, e.g.
/// `pixelfed://comments/$route.comment`) is scrolled into view the first time
/// the list draws it, and drawn on `background` with `content_color` (both
/// `#RRGGBB`, the full item width). No item with that URI: nothing happens.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ListFocus {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_color: Option<String>,
}

/// `core://Text` `format` (feature `date_format`,
/// docs/decisions/text-dates-and-counts.md): every value the text binds that
/// is a `YYYY-MM-DD` date is drawn as `date` says, against the viewer's own
/// clock and time zone (the pod knows neither, and a cached screen must stay
/// right the next day). Other values are drawn as they are. With `past`, a
/// text binding a date before today draws `past` instead ("Overdue").
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TextFormat {
    pub date: DateFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub past: Option<String>,
}

/// How [`TextFormat`] draws a date.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateFormat {
    /// "today", "tomorrow", "yesterday", "in 3 days", "3 days ago" (up to
    /// six days either way), else as `Short`.
    Relative,
    /// "Nov 5", with the year when it is not this year ("Nov 5, 2027").
    Short,
}

/// `core://Text` `count` (feature `count`,
/// docs/decisions/text-dates-and-counts.md): the number of entities of
/// `queries[query_key]` that match every `where` (as a `core://List`'s
/// [`ListWhere`]: `equals` may be `$form.<field>`, and a blank one matches
/// everything) replaces each `$count` in `value_expr`. A count of 0 draws
/// `zero_text` instead, when set. While the query is not loaded the text is
/// empty.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TextCount {
    pub query_key: String,
    #[serde(rename = "where", default, skip_serializing_if = "Vec::is_empty")]
    pub where_: Vec<ListWhere>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zero_text: Option<String>,
}

/// One `core://ChoiceChips` option: `label` is drawn, `value` is stored.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ChoiceOption {
    pub label: String,
    #[serde(default)]
    pub value: String,
}

/// Where `core://Box` places its content (schema 6).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

fn is_start(a: &Align) -> bool {
    *a == Align::Start
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageShape {
    #[default]
    Square,
    Rounded,
    Circle,
}

/// A tab of a `core://Dock`: what its button shows (`label`, an `icon`
/// name, an optional `badge` binding drawn as a count or a dot) and the
/// `content` shown while it is selected. In a dock's `item`, every field may
/// bind `$item.<field>`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct DockTab {
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    pub content: Box<UiNode>,
}

/// A page of a `core://Drawer`: the `content` shown while its `value` is
/// selected. In a drawer's `item`, `value` may bind `$item.<field>`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct DrawerPage {
    pub value: String,
    pub content: UiNode,
}

/// A named region of a workspace. `provider_uri` is a label only: nothing
/// reads it (optional, decision `plumbing-and-porcelain`); it is still
/// written, as `""` when unset, because older clients require it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SlotSpec {
    pub slot_id: String,
    #[serde(default)]
    pub provider_uri: String,
    pub content: UiNode,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Vertical,
    Horizontal,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextStyle {
    Headline,
    Title,
    Body,
    Caption,
    /// Semi-bold, small title size (schema 3).
    Label,
    /// Small and muted (schema 3).
    Secondary,
    /// A large serif title (schema 6, D44).
    Display,
}

/// `core://Chip` colour.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    #[default]
    Neutral,
    Warning,
}

/// A list of strings that may be given literally or as a binding expression
/// resolved against the current item / entity at render time.
///
/// Serialized untagged so the original wire form is preserved:
/// `"$item.media_uris"` <-> `Expr`, `["pixelfed://media/1"]` <-> `List`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum ListBinding {
    Expr(String),
    List(Vec<String>),
}

impl ListBinding {
    /// The binding expression, if this is an `Expr`.
    pub fn as_expr(&self) -> Option<&str> {
        match self {
            ListBinding::Expr(e) => Some(e),
            ListBinding::List(_) => None,
        }
    }

    /// The literal list, if this is a `List`.
    pub fn as_list(&self) -> Option<&[String]> {
        match self {
            ListBinding::Expr(_) => None,
            ListBinding::List(l) => Some(l),
        }
    }
}

impl From<Vec<String>> for ListBinding {
    fn from(v: Vec<String>) -> Self {
        ListBinding::List(v)
    }
}

impl From<&str> for ListBinding {
    fn from(s: &str) -> Self {
        ListBinding::Expr(s.to_owned())
    }
}

impl From<String> for ListBinding {
    fn from(s: String) -> Self {
        ListBinding::Expr(s)
    }
}

/// How a `core://Layout` shows its layout (decision `layouts-not-apps`).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmbedPresentation {
    /// In place, in the space the parent gives it.
    #[default]
    Inline,
    /// A bottom sheet over the screen.
    Sheet,
    /// A side panel over the screen, from the start edge.
    Panel,
    /// Over the screen from its `edge` (feature `overlay_edge`).
    Overlay,
}

impl EmbedPresentation {
    fn is_inline(&self) -> bool {
        *self == EmbedPresentation::Inline
    }
}
