package org.theconstruct

import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.descriptors.buildClassSerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonDecoder
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonEncoder
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlin.reflect.KClass

// Kotlin mirror of crates/protocol (ast.rs, intent.rs, bundle.rs) and docs/layouts.md.
// Field names are snake_case on the wire via @SerialName.

// ---------------------------------------------------------------------------
// Workspace bundle (DECISIONS D2)
// ---------------------------------------------------------------------------

@Serializable
data class WorkspaceSpec(
    val version: String,
    val workspace: UiNode,
    val entities: Map<String, JsonObject> = emptyMap(),
    /** query_key -> ordered entity URIs (M6); omitted when empty, like serde skip_serializing_if. */
    val queries: Map<String, List<String>> = emptyMap(),
)

/** A workspace slot. [providerUri] is a label nothing reads (optional; decision `plumbing-and-porcelain`). */
@Serializable
data class SlotSpec(
    @SerialName("slot_id") val slotId: String,
    @SerialName("provider_uri") val providerUri: String = "",
    val content: UiNode,
)

@Serializable
enum class Orientation { Vertical, Horizontal }

@Serializable
enum class TextStyle {
    Headline, Title, Body, Caption, Label, Secondary,

    /** A large serif title (schema 6, D44). */
    Display,
}

/** Where `core://Box` puts its content, how a `core://Column` places its children, or a `core://Row`'s vertical alignment (schema 6, D44). */
@Serializable
enum class Align { Start, Center, End }

/** `core://Chip` colour. */
@Serializable
enum class Tone { Neutral, Warning }

/**
 * How `action.navigation.open` presents: a screen as Push, Modal or Replace; another layout (decision
 * `layouts-not-apps`) as Push (the whole screen), Sheet (a bottom sheet), Panel (a side panel from the start edge) or
 * Overlay (from its [Edge], feature `overlay_edge`).
 */
@Serializable
enum class PresentationMode { Push, Modal, Replace, Sheet, Panel, Overlay }

/** How a `core://Layout` shows its layout: in place, or over the screen as a sheet, a side panel or an overlay. */
@Serializable
enum class EmbedPresentation { Inline, Sheet, Panel, Overlay }

/**
 * The edge an overlay comes in from (feature `overlay_edge`, decision `edge-overlays-and-gestures`); dragging it back
 * toward that edge dismisses it. A Sheet is an overlay from the bottom, a Panel one from the start.
 */
@Serializable
enum class Edge {
    @SerialName("top") Top,
    @SerialName("bottom") Bottom,
    @SerialName("start") Start,
    @SerialName("end") End,
}

/** How a presented layout comes in and goes out (named only; the platform picks durations). */
@Serializable
enum class Animation {
    @SerialName("none") None,
    @SerialName("fade") Fade,
    @SerialName("slide") Slide,
    @SerialName("sheet_up") SheetUp,
}

@Serializable
enum class ImageShape { Square, Rounded, Circle }

/** Where `action.mutation.create` lists its new entity (schema 9): at the start or (default) the end of its query. */
@Serializable
enum class CreatePosition {
    @SerialName("start") Start,
    @SerialName("end") End,
}

/**
 * A screen an app can open (DECISIONS D34; protocol `ast::Screen`). [route] is a URI pattern with `{name}` segments;
 * `$route.<name>` in the other fields is replaced by the captured value. [data] is a pod path loaded when the screen
 * opens; [footer] stays at the bottom while [content] scrolls.
 */
@Serializable
data class Screen(
    val id: String,
    val route: String,
    val title: String = "",
    val data: String? = null,
    /** A workspace screen's content (the app draws the screen chrome around it). */
    val content: UiNode? = null,
    val footer: UiNode? = null,
    /**
     * A `core://Page` screen (feature `page`): the whole screen, drawn with no chrome from the app; [content] and
     * [footer] are then unset.
     */
    val root: UiNode? = null,
    /** Feature `screen_item`: `$item` throughout a page screen is the entity at this URI. */
    val item: String? = null,
)

/** `core://List` indentation: `item[field] * stepDp`, at most [max] steps (default 4). */
@Serializable
data class Indent(
    val field: String,
    @SerialName("step_dp") val stepDp: Int,
    val max: Int? = null,
)

/**
 * `core://List` search (schema 5, D43). [value] is `$form.<field>` or literal text; while it is non-blank only items
 * where every word of it is found in one of [fields] are drawn (case and accents ignored, [matchesFilter]).
 * [noMatchText] shows when nothing matches (default: the list's `empty_text`).
 */
@Serializable
data class ListFilter(
    val value: String,
    val fields: List<String>,
    @SerialName("no_match_text") val noMatchText: String? = null,
)

/**
 * `core://List` group headers (schema 5, D43): [header] is drawn before each drawn item whose [field] changes. From
 * schema 6 (D44) the header's `$item` also has `group_count`, the number of drawn items in its group.
 */
@Serializable
data class GroupBy(
    val field: String,
    val header: UiNode,
    /** Feature `list_group_headers`: only the headers are drawn, one per distinct value. */
    @SerialName("headers_only") val headersOnly: Boolean = false,
)

/**
 * `core://Text` `format` (feature `date_format`, docs/decisions/text-dates-and-counts.md): every bound value that is a
 * `YYYY-MM-DD` date is drawn as [date] says, against the device's clock and time zone. With [past], a text binding a
 * date before today draws [past] instead ("Overdue").
 */
@Serializable
data class TextFormat(val date: DateFormat, val past: String? = null)

/** How [TextFormat] draws a date: [Relative] ("tomorrow", "in 3 days", else as [Short]) or [Short] ("Nov 5"). */
@Serializable
enum class DateFormat { Relative, Short }

/**
 * `core://Text` `count` (feature `count`): the number of entities of `queries[queryKey]` matching every [where] (as a
 * list's [ListWhere]) replaces each `$count`; a count of 0 draws [zeroText] instead, when set.
 */
@Serializable
data class TextCount(
    @SerialName("query_key") val queryKey: String,
    val where: List<ListWhere> = emptyList(),
    @SerialName("zero_text") val zeroText: String? = null,
)

/** `core://List` `where` (schema 6, D44): while [equals] (`$form.<field>` or literal) is non-blank, only items whose [field] equals it are drawn. */
@Serializable
data class ListWhere(val field: String, val equals: String)

/**
 * A `core://List` `where`: one condition (`{field, equals}`) or, feature `list_where_all` (decision `list-where-all`),
 * a list of them that must all match, like a [TextCount]'s. Read as a list either way; one condition is written back
 * as an object.
 */
object WhereClauseSerializer : KSerializer<List<ListWhere>> {
    override val descriptor: SerialDescriptor = buildClassSerialDescriptor("org.theconstruct.WhereClause")

    override fun deserialize(decoder: Decoder): List<ListWhere> {
        val input = decoder as? JsonDecoder ?: throw SerializationException("WhereClause supports JSON only")
        return when (val e = input.decodeJsonElement()) {
            is JsonObject -> listOf(input.json.decodeFromJsonElement(ListWhere.serializer(), e))
            is JsonArray -> e.map { input.json.decodeFromJsonElement(ListWhere.serializer(), it) }
            else -> throw SerializationException("a list where must be an object or an array of objects, got $e")
        }
    }

    override fun serialize(encoder: Encoder, value: List<ListWhere>) {
        val output = encoder as? JsonEncoder ?: throw SerializationException("WhereClause supports JSON only")
        output.encodeJsonElement(
            value.singleOrNull()?.let { output.json.encodeToJsonElement(ListWhere.serializer(), it) }
                ?: JsonArray(value.map { output.json.encodeToJsonElement(ListWhere.serializer(), it) })
        )
    }
}

/**
 * `core://List` `focus` (schema 12, docs/decisions/list-focus.md): the item whose entity URI is [uri] is scrolled into
 * view the first time the list draws it, and drawn on [background] with [contentColor] (`#RRGGBB`).
 */
@Serializable
data class ListFocus(
    val uri: String,
    val background: String? = null,
    @SerialName("content_color") val contentColor: String? = null,
)

/** One `core://ChoiceChips` option (schema 6, D44): [label] is drawn, [value] is stored. */
@Serializable
data class ChoiceOption(val label: String, val value: String = "")

// ---------------------------------------------------------------------------
// UI AST, discriminated by "type"
// ---------------------------------------------------------------------------

@Serializable(with = UiNodeSerializer::class)
sealed class UiNode {
    @Serializable
    data class Workspace(
        val id: String,
        val title: String,
        val slots: List<SlotSpec>,
        /** Screens this app can open (D34); omitted when empty. */
        val screens: List<Screen> = emptyList(),
        /** The title scrolls away with the content (feature `title_scrolls`); by default it is pinned in a top bar. */
        @SerialName("title_scrolls") val titleScrolls: Boolean = false,
    ) : UiNode()

    /**
     * `core://Page` (feature `page`, decision `plumbing-and-porcelain`): a layout whose [root] is any node, drawn over
     * the whole space with no chrome from the app ([NodePage]). [title] names it and is not drawn. [screens] open by
     * route and draw their own [Screen.root].
     */
    @Serializable
    data class Page(
        val id: String,
        val title: String = "",
        val root: UiNode,
        val screens: List<Screen> = emptyList(),
    ) : UiNode()

    /**
     * `core://Scroll` (feature `scroll`): [content] scrolls, filling the height its parent leaves ([NodeScroll]); with
     * [followEnd] it follows its content to the end after an action of the page.
     */
    @Serializable
    data class Scroll(
        val content: UiNode,
        @SerialName("follow_end") val followEnd: Boolean = false,
    ) : UiNode()

    @Serializable
    data class Container(
        val orientation: Orientation,
        @SerialName("padding_dp") val paddingDp: Int,
        val children: List<UiNode>,
    ) : UiNode()

    /**
     * Text. [format] (feature `date_format`) draws the `YYYY-MM-DD` dates it binds for the viewer; [count] (feature
     * `count`) puts a count of a query's entities in place of `$count` ([formatText]).
     */
    @Serializable
    data class Text(
        @SerialName("value_expr") val valueExpr: String,
        val style: TextStyle,
        val format: TextFormat? = null,
        val count: TextCount? = null,
    ) : UiNode()

    @Serializable
    data class Button(
        val label: String,
        @SerialName("on_click") val onClick: ActionIntent,
    ) : UiNode()

    /**
     * `core://List` (D34): the entities of `queries[queryKey]`, each rendered with [itemTemplate]. [filter] and
     * [groupBy] are schema 5 (D43); [columns], [spacingDp] and [paddingHDp] schema 8 (D48).
     */
    @Serializable
    data class ItemList(
        @SerialName("query_key") val queryKey: String,
        @SerialName("item_template") val itemTemplate: UiNode,
        val indent: Indent? = null,
        @SerialName("empty_text") val emptyText: String? = null,
        val filter: ListFilter? = null,
        @SerialName("group_by") val groupBy: GroupBy? = null,
        /** Schema 6 (D44): conditions that must all match ([WhereClauseSerializer]; several: `list_where_all`). */
        @SerialName("where") @Serializable(with = WhereClauseSerializer::class) val where: List<ListWhere> = emptyList(),
        /** Schema 8 (D48): items in this many equal columns (a grid); 0 and 1 mean one per row. */
        val columns: Int = 0,
        /** Schema 8 (D48): space between items (both ways in a grid). */
        @SerialName("spacing_dp") val spacingDp: Int = 0,
        /** Schema 8 (D48): space left and right of the items. */
        @SerialName("padding_h_dp") val paddingHDp: Int = 0,
        /** Schema 12: one item to scroll to and tint ([ListFocus]). */
        val focus: ListFocus? = null,
    ) : UiNode()

    /**
     * `core://Image` (D34): [url] is a literal or a binding such as `$item.avatar_url`. Without [sizeDp] it fills a
     * carousel page, else the width at [aspectRatio] (default 1). [blurhash] colours the placeholder; on error the
     * [contentDescription] shows.
     */
    @Serializable
    data class Image(
        val url: String,
        val shape: ImageShape = ImageShape.Square,
        @SerialName("size_dp") val sizeDp: Int? = null,
        @SerialName("aspect_ratio") val aspectRatio: Float? = null,
        @SerialName("content_description") val contentDescription: String? = null,
        val blurhash: String? = null,
    ) : UiNode()

    /**
     * `core://TextField` (D34): edits the form value `$form.<field>`; [onSubmit] is sent with it, then it clears.
     * Without [onSubmit] (e.g. a search box feeding a list's filter, D43) it shows a Clear button while non-empty.
     */
    @Serializable
    data class TextField(
        val field: String,
        val placeholder: String = "",
        @SerialName("max_length") val maxLength: Int? = null,
        @SerialName("on_submit") val onSubmit: ActionIntent? = null,
        /** Schema 9 (D51): without [onSubmit], a field of up to this many lines (e.g. a caption). */
        val lines: Int? = null,
    ) : UiNode()

    // Schema 3 (DECISIONS D36): generic building blocks for composing any app on the pod.

    /**
     * Children side by side, not wrapping; a [Spacer] without a size takes the rest. Vertically centred unless [align]
     * (schema 6) says `Start` (top) or `End` (bottom).
     */
    @Serializable
    data class Row(
        val children: List<UiNode>,
        @SerialName("spacing_dp") val spacingDp: Int = 0,
        @SerialName("padding_h_dp") val paddingHDp: Int = 0,
        @SerialName("padding_v_dp") val paddingVDp: Int = 0,
        val align: Align? = null,
    ) : UiNode()

    /** Children stacked, full width; [align] (schema 6) places narrower children. */
    @Serializable
    data class Column(
        val children: List<UiNode>,
        @SerialName("spacing_dp") val spacingDp: Int = 0,
        @SerialName("padding_h_dp") val paddingHDp: Int = 0,
        @SerialName("padding_v_dp") val paddingVDp: Int = 0,
        val align: Align = Align.Start,
    ) : UiNode()

    /** Empty space: [sizeDp], or (null, in a [Row]) the remaining width. */
    @Serializable
    data class Spacer(@SerialName("size_dp") val sizeDp: Int? = null) : UiNode()

    /** A thin horizontal line. */
    @Serializable
    data object Divider : UiNode()

    /**
     * [content] only when [value] (e.g. `$item.caption`, or `$form.<field>` from schema 8) is truthy ([isTruthy]), or
     * with [equals] (schema 8, D48) when the value as text equals it ([conditionShows]).
     */
    @Serializable
    data class Conditional(val value: String, val content: UiNode, val equals: String? = null) : UiNode()

    /** A pager over the entities [items] names, each drawn with [itemTemplate]; see protocol `core://Carousel`. */
    @Serializable
    data class Carousel(
        val items: ListBinding,
        @SerialName("item_template") val itemTemplate: UiNode,
        @SerialName("aspect_ratio") val aspectRatio: Double = 1.0,
        @SerialName("aspect_field") val aspectField: String? = null,
        @SerialName("min_aspect") val minAspect: Double? = null,
        @SerialName("max_aspect") val maxAspect: Double? = null,
        val tag: String? = null,
    ) : UiNode()

    /** An icon button; [active] (a binding to a bool) switches to the active icon, tint and description. */
    @Serializable
    data class IconButton(
        val icon: String,
        @SerialName("content_description") val contentDescription: String,
        @SerialName("on_click") val onClick: ActionIntent,
        val active: String? = null,
        @SerialName("active_icon") val activeIcon: String? = null,
        @SerialName("active_tint") val activeTint: String? = null,
        @SerialName("active_content_description") val activeContentDescription: String? = null,
        @SerialName("size_dp") val sizeDp: Int? = null,
        val tag: String? = null,
    ) : UiNode()

    /** A small label with an optional icon, coloured by [tone]. */
    @Serializable
    data class Chip(val text: String, val icon: String? = null, val tone: Tone = Tone.Neutral) : UiNode()

    /** A one-line note with a small icon, e.g. "Bob boosted". */
    @Serializable
    data class Badge(val text: String, val icon: String) : UiNode()

    /**
     * [content] made tappable (schema 4, D42): a tap anywhere on it dispatches [onClick]; a button inside keeps its own
     * tap. [contentDescription] is for screen readers; [tag] (bindable) names it for tests.
     */
    @Serializable
    data class Tappable(
        @SerialName("on_click") val onClick: ActionIntent,
        val content: UiNode,
        @SerialName("content_description") val contentDescription: String? = null,
        val tag: String? = null,
    ) : UiNode()

    /**
     * [content] on a coloured, rounded surface (schema 6, D44). Colours are `#RRGGBB` or a binding resolving to one
     * ([interpolate]); [contentColor] colours the text and icons inside. Without [widthDp] / [heightDp] the box wraps
     * its content; with them, [align] places it.
     */
    @Serializable
    data class Box(
        val content: UiNode,
        val background: String? = null,
        @SerialName("content_color") val contentColor: String? = null,
        @SerialName("border_color") val borderColor: String? = null,
        @SerialName("corner_dp") val cornerDp: Int = 0,
        @SerialName("padding_h_dp") val paddingHDp: Int = 0,
        @SerialName("padding_v_dp") val paddingVDp: Int = 0,
        @SerialName("width_dp") val widthDp: Int? = null,
        @SerialName("height_dp") val heightDp: Int? = null,
        val align: Align = Align.Start,
        /** Feature `box_fill`: fill the space given, [content] placed by [align] in both directions. */
        val fill: Boolean = false,
    ) : UiNode()

    /**
     * A sideways-scrolling row of chips editing the form value `$form.<field>` (schema 6, D44): a tap sets it to the
     * option's value (empty clears it); the matching option (the empty one while unset) is selected. With [remember]
     * (schema 10) the device keeps the choice and restores it when the layout or screen opens again
     * ([rememberedChoices], docs/decisions/remembered-form-values.md).
     */
    @Serializable
    data class ChoiceChips(
        val field: String,
        val options: List<ChoiceOption>,
        @SerialName("padding_h_dp") val paddingHDp: Int = 0,
        val remember: Boolean = false,
    ) : UiNode()

    /**
     * `core://MediaPicker` (schema 9, D49/D51): thumbnails of the photos picked into the form value `$form.<field>`
     * (a JSON array of staged attachment ids, see [parseFormList]) with a remove button each, and an add tile
     * ([label], default "Add photo") while fewer than [maxItems] are picked.
     */
    @Serializable
    data class MediaPicker(
        val field: String,
        @SerialName("max_items") val maxItems: Int,
        val label: String? = null,
    ) : UiNode()

    /**
     * `core://Sticky` (schema 11): [content] scrolls with the page until it reaches the top of the scrolling view, then
     * stays pinned there (on the page background) while the rest of its parent scrolls under it ([StickyScroll]).
     */
    @Serializable
    data class Sticky(val content: UiNode) : UiNode()

    /**
     * `core://Progress` (feature `progress`): an indeterminate circular spinner [sizeDp] across (default
     * [PROGRESS_DEFAULT_SIZE_DP]) for work in progress, e.g. inside a [Conditional] on a status field.
     */
    @Serializable
    data class Progress(@SerialName("size_dp") val sizeDp: Int? = null) : UiNode()

    /**
     * `core://Layout` (feature `layout_embed`, decision `layouts-not-apps`): another layout drawn here ([LayoutNodeView]).
     * [presentation] Inline (default) draws it in place, [heightDp] tall when set; Sheet and Panel present it over the
     * screen. With [showWhen] it shows only while that binding is truthy (or equals [equals]); dismissing a presented
     * one clears the `$form` field [showWhen] names. [params] bind as `$route.<name>` in it.
     */
    @Serializable
    data class Layout(
        val layout: String,
        @SerialName("show_when") val showWhen: String? = null,
        val equals: String? = null,
        val presentation: EmbedPresentation = EmbedPresentation.Inline,
        val animation: Animation? = null,
        val params: Map<String, String> = emptyMap(),
        @SerialName("height_dp") val heightDp: Int? = null,
        /** With [EmbedPresentation.Overlay]: the edge it comes in from (bottom when null). */
        val edge: Edge? = null,
        /** With [EmbedPresentation.Overlay]: its height (top, bottom) or width (start, end). */
        @SerialName("size_dp") val sizeDp: Int? = null,
    ) : UiNode()

    /**
     * `core://Gesture` (feature `gesture`, decision `edge-overlays-and-gestures`): [content] with gesture handlers, each
     * an intent ([GestureNodeView]). A swipe is a drag mostly one way; a pull the content can't scroll for counts.
     */
    @Serializable
    data class Gesture(
        val content: UiNode,
        @SerialName("on_tap") val onTap: ActionIntent? = null,
        @SerialName("on_long_press") val onLongPress: ActionIntent? = null,
        @SerialName("on_swipe_up") val onSwipeUp: ActionIntent? = null,
        @SerialName("on_swipe_down") val onSwipeDown: ActionIntent? = null,
        @SerialName("on_swipe_left") val onSwipeLeft: ActionIntent? = null,
        @SerialName("on_swipe_right") val onSwipeRight: ActionIntent? = null,
        @SerialName("content_description") val contentDescription: String? = null,
    ) : UiNode()

    /** `core://Switch` (feature `switch`): on while [checked] (a binding) is truthy; a tap dispatches [onClick]. */
    @Serializable
    data class Switch(
        val checked: String,
        @SerialName("on_click") val onClick: ActionIntent,
        @SerialName("content_description") val contentDescription: String? = null,
    ) : UiNode()

    /** A node whose `type` this client does not know. [raw] is the full original object. */
    data class Unknown(val type: String, val raw: JsonObject) : UiNode()
}

// ---------------------------------------------------------------------------
// Intents, discriminated by "intent"
// ---------------------------------------------------------------------------

@Serializable(with = ActionIntentSerializer::class)
sealed class ActionIntent {
    @Serializable
    data class Navigate(
        @SerialName("target_uri") val targetUri: String = "",
        val presentation: PresentationMode,
        /** Another layout to present (feature `navigate_layout`); then [targetUri], if set, opens its screen. */
        val layout: String? = null,
        /** `$route.<name>` values of the presented [layout]. */
        val params: Map<String, String> = emptyMap(),
        val animation: Animation? = null,
        /** With [PresentationMode.Overlay]: the edge it comes in from (bottom when null). */
        val edge: Edge? = null,
        /** With [PresentationMode.Overlay]: its height (top, bottom) or width (start, end). */
        @SerialName("size_dp") val sizeDp: Int? = null,
    ) : ActionIntent()

    @Serializable
    data object NavigateBack : ActionIntent()

    @Serializable
    data class OptimisticMutate(
        @SerialName("entity_uri") val entityUri: String,
        val field: String,
        @SerialName("value_expr") val valueExpr: String,
        @SerialName("rollback_value") val rollbackValue: JsonElement,
        val endpoint: String,
        val payload: JsonElement,
        /**
         * Extra fields of the same entity, evaluated against the PRE-mutation entity and applied
         * (and rolled back) together with [field]. Omitted when empty (encodeDefaults = false).
         */
        val also: List<FieldUpdate> = emptyList(),
    ) : ActionIntent()

    /** Sign in; [appPath] / [tokenPath] are the pod's sign-in endpoints (D36). */
    @Serializable
    data class AuthLogin(
        @SerialName("instance_url") val instanceUrl: String,
        @SerialName("app_path") val appPath: String = "",
        @SerialName("token_path") val tokenPath: String = "",
    ) : ActionIntent()

    /**
     * `action.auth.session` (schema 7, D46): the engine POSTs [payload] to the server path [path] and keeps the
     * answer's `session_token` as its session, e.g. a typed pod code (`/v1/pods/open`) or a temporary pod.
     */
    @Serializable
    data class OpenSession(
        val path: String,
        val payload: JsonElement = JsonObject(emptyMap()),
    ) : ActionIntent()

    /** `action.data.load` (D34): the engine loads a pod path (a screen's `data`) and caches its answer. */
    @Serializable
    data class LoadData(val path: String) : ActionIntent()

    /**
     * `action.mutation.create` (D34): the engine lists [entity] at [entityUri] in `queries[queryKey]` at once and sends
     * [payload] to [endpoint] through the outbox.
     *
     * Schema 9 (D49, D51): [attachments] maps a name to a form-bound list of staged attachment ids (`{"photos":
     * "$form.photos"}`, bound by [bindForm] to a JSON array); the engine uploads them first and replaces
     * `"$attachments.<name>"` in [entity] (local attachment URIs) and [payload] (upload ids) itself. [position] puts the
     * entity at the start or end of the list. (Schema 13 removed `dismiss`: layouts close the screen with
     * `action.sequence [this create, action.navigation.back]`; decision `named-vocabulary`.)
     */
    @Serializable
    data class Create(
        @SerialName("query_key") val queryKey: String,
        @SerialName("entity_uri") val entityUri: String,
        val entity: JsonElement,
        val endpoint: String,
        val payload: JsonElement,
        @SerialName("refresh_workspace") val refreshWorkspace: Boolean = false,
        val attachments: Map<String, JsonElement> = emptyMap(),
        val position: CreatePosition = CreatePosition.End,
    ) : ActionIntent()

    /**
     * `action.sequence` (feature `action_sequence`): run [steps] in order as one tap. The whole sequence is bound at
     * once (one `$nonce`), then each step is dispatched as if tapped alone: navigation steps here, the rest by the
     * engine.
     */
    @Serializable
    data class Sequence(val steps: List<ActionIntent>) : ActionIntent()

    /**
     * `action.local.set` (feature `local_set`): the engine sets [field] (and [also]) of [entityUri] on this device
     * only, from [valueExpr] evaluated against the entity; nothing is sent, and the next load replaces it.
     */
    @Serializable
    data class LocalSet(
        @SerialName("entity_uri") val entityUri: String,
        val field: String,
        @SerialName("value_expr") val valueExpr: String,
        val also: List<FieldUpdate> = emptyList(),
    ) : ActionIntent()

    /**
     * `action.link.open` (feature `open_link`): open [url] (`http(s)://` only) in the device's browser. Handled here,
     * never sent to the engine.
     */
    @Serializable
    data class OpenLink(val url: String) : ActionIntent()

    /**
     * `action.device` (feature `device_action`): something the device does itself ([command] with its [app] (a layout)
     * or [id]: `open_notification`, `mark_all_read`, `toggle_channel`, `sign_in`, `sign_out`, `set_server`,
     * `show_versions`, `pin_version`, `follow_latest`, `leave_pod`, `refresh`, `open_settings`, `open_inbox`,
     * `request_notification_permission`). Handled
     * by [ConstructController], never sent to the engine.
     */
    @Serializable
    data class Device(
        val command: String,
        val app: String? = null,
        val id: String? = null,
        val value: String? = null,
    ) : ActionIntent()

    /**
     * `action.confirm` (feature `action_confirm`): ask before a step that can't be undone. [ConstructController] shows
     * its own dialog ([title], [message], a [confirmLabel] button and Cancel) and runs [then] only on confirm; in a
     * sequence, the steps after it wait for the answer too. Never sent to the engine.
     */
    @Serializable
    data class Confirm(
        val title: String,
        val message: String? = null,
        @SerialName("confirm_label") val confirmLabel: String,
        val then: ActionIntent,
    ) : ActionIntent()

    /**
     * An intent whose `intent` tag this client does not know (e.g. the removed `action.agent.prompt`). [raw] is the full
     * original object; dispatching it forwards [raw] unchanged.
     */
    data class Unknown(val intent: String, val raw: JsonObject) : ActionIntent()
}

/** One extra field update in [ActionIntent.OptimisticMutate.also]. */
@Serializable
data class FieldUpdate(
    val field: String,
    @SerialName("value_expr") val valueExpr: String,
)

// ---------------------------------------------------------------------------
// ListBinding (DECISIONS D1: protocol::ListBinding, untagged)
// ---------------------------------------------------------------------------

@Serializable(with = ListBindingSerializer::class)
sealed class ListBinding {
    /** A binding expression such as `"$item.media_uris"`. */
    data class Expr(val expr: String) : ListBinding()

    /** A literal list of entity URIs. */
    data class Items(val items: List<String>) : ListBinding()
}

object ListBindingSerializer : KSerializer<ListBinding> {
    override val descriptor: SerialDescriptor = buildClassSerialDescriptor("org.theconstruct.ListBinding")

    override fun deserialize(decoder: Decoder): ListBinding {
        val input = decoder as? JsonDecoder ?: throw SerializationException("ListBinding supports JSON only")
        val e = input.decodeJsonElement()
        return when {
            e is JsonPrimitive && e.isString -> ListBinding.Expr(e.content)
            e is JsonArray -> ListBinding.Items(
                e.map { (it as? JsonPrimitive)?.takeIf { p -> p.isString }?.content
                    ?: throw SerializationException("a list binding array must contain strings, got $it") }
            )
            else -> throw SerializationException("a list binding must be a string or an array of strings, got $e")
        }
    }

    override fun serialize(encoder: Encoder, value: ListBinding) {
        val output = encoder as? JsonEncoder ?: throw SerializationException("ListBinding supports JSON only")
        output.encodeJsonElement(
            when (value) {
                is ListBinding.Expr -> JsonPrimitive(value.expr)
                is ListBinding.Items -> JsonArray(value.items.map(::JsonPrimitive))
            }
        )
    }
}

// ---------------------------------------------------------------------------
// Tagged serializers with Unknown fallback
// ---------------------------------------------------------------------------

/**
 * Serde-style internally tagged enum: reads the object, dispatches on [key], and decodes the
 * concrete variant from the object with [key] removed. On encode, writes [key] first.
 */
abstract class TaggedUnionSerializer<T : Any>(serialName: String, private val key: String) : KSerializer<T> {
    protected class Variant<T : Any>(val tag: String, val kClass: KClass<out T>, val serializer: KSerializer<out T>)

    protected abstract val variants: List<Variant<T>>
    protected abstract fun unknown(tag: String, raw: JsonObject): T
    protected abstract fun unknownRaw(value: T): JsonObject?

    private val byTag by lazy { variants.associateBy { it.tag } }
    private val byClass by lazy { variants.associateBy { it.kClass } }

    override val descriptor: SerialDescriptor = buildClassSerialDescriptor(serialName)

    override fun deserialize(decoder: Decoder): T {
        val input = decoder as? JsonDecoder ?: throw SerializationException("$key-tagged union supports JSON only")
        val obj = input.decodeJsonElement().jsonObject
        val tag = (obj[key] as? JsonPrimitive)?.contentOrNull ?: ""
        val variant = byTag[tag] ?: return unknown(tag, obj)
        return input.json.decodeFromJsonElement(variant.serializer, JsonObject(obj - key))
    }

    @Suppress("UNCHECKED_CAST")
    override fun serialize(encoder: Encoder, value: T) {
        val output = encoder as? JsonEncoder ?: throw SerializationException("$key-tagged union supports JSON only")
        unknownRaw(value)?.let { output.encodeJsonElement(it); return }
        val variant = byClass[value::class] ?: throw SerializationException("No variant registered for ${value::class}")
        val body = output.json.encodeToJsonElement(variant.serializer as KSerializer<T>, value).jsonObject
        output.encodeJsonElement(JsonObject(linkedMapOf<String, JsonElement>(key to JsonPrimitive(variant.tag)) + body))
    }
}

object UiNodeSerializer : TaggedUnionSerializer<UiNode>("org.theconstruct.UiNode", "type") {
    override val variants = listOf<Variant<UiNode>>(
        Variant("core://Workspace", UiNode.Workspace::class, UiNode.Workspace.serializer()),
        Variant("core://Page", UiNode.Page::class, UiNode.Page.serializer()),
        Variant("core://Scroll", UiNode.Scroll::class, UiNode.Scroll.serializer()),
        Variant("core://Container", UiNode.Container::class, UiNode.Container.serializer()),
        Variant("core://Text", UiNode.Text::class, UiNode.Text.serializer()),
        Variant("core://Button", UiNode.Button::class, UiNode.Button.serializer()),
        Variant("core://List", UiNode.ItemList::class, UiNode.ItemList.serializer()),
        Variant("core://Image", UiNode.Image::class, UiNode.Image.serializer()),
        Variant("core://TextField", UiNode.TextField::class, UiNode.TextField.serializer()),
        Variant("core://Row", UiNode.Row::class, UiNode.Row.serializer()),
        Variant("core://Column", UiNode.Column::class, UiNode.Column.serializer()),
        Variant("core://Spacer", UiNode.Spacer::class, UiNode.Spacer.serializer()),
        Variant("core://Divider", UiNode.Divider::class, UiNode.Divider.serializer()),
        Variant("core://Conditional", UiNode.Conditional::class, UiNode.Conditional.serializer()),
        Variant("core://Carousel", UiNode.Carousel::class, UiNode.Carousel.serializer()),
        Variant("core://IconButton", UiNode.IconButton::class, UiNode.IconButton.serializer()),
        Variant("core://Chip", UiNode.Chip::class, UiNode.Chip.serializer()),
        Variant("core://Badge", UiNode.Badge::class, UiNode.Badge.serializer()),
        Variant("core://Tappable", UiNode.Tappable::class, UiNode.Tappable.serializer()),
        Variant("core://Box", UiNode.Box::class, UiNode.Box.serializer()),
        Variant("core://ChoiceChips", UiNode.ChoiceChips::class, UiNode.ChoiceChips.serializer()),
        Variant("core://MediaPicker", UiNode.MediaPicker::class, UiNode.MediaPicker.serializer()),
        Variant("core://Sticky", UiNode.Sticky::class, UiNode.Sticky.serializer()),
        Variant("core://Progress", UiNode.Progress::class, UiNode.Progress.serializer()),
        Variant("core://Switch", UiNode.Switch::class, UiNode.Switch.serializer()),
        Variant("core://Layout", UiNode.Layout::class, UiNode.Layout.serializer()),
        Variant("core://Gesture", UiNode.Gesture::class, UiNode.Gesture.serializer()),
    )

    override fun unknown(tag: String, raw: JsonObject): UiNode = UiNode.Unknown(tag, raw)
    override fun unknownRaw(value: UiNode): JsonObject? = (value as? UiNode.Unknown)?.raw
}

object ActionIntentSerializer : TaggedUnionSerializer<ActionIntent>("org.theconstruct.ActionIntent", "intent") {
    override val variants = listOf<Variant<ActionIntent>>(
        Variant("action.navigation.open", ActionIntent.Navigate::class, ActionIntent.Navigate.serializer()),
        Variant("action.navigation.back", ActionIntent.NavigateBack::class, ActionIntent.NavigateBack.serializer()),
        Variant("action.mutation.optimistic", ActionIntent.OptimisticMutate::class, ActionIntent.OptimisticMutate.serializer()),
        Variant("action.auth.login", ActionIntent.AuthLogin::class, ActionIntent.AuthLogin.serializer()),
        Variant("action.auth.session", ActionIntent.OpenSession::class, ActionIntent.OpenSession.serializer()),
        Variant("action.data.load", ActionIntent.LoadData::class, ActionIntent.LoadData.serializer()),
        Variant("action.mutation.create", ActionIntent.Create::class, ActionIntent.Create.serializer()),
        Variant("action.sequence", ActionIntent.Sequence::class, ActionIntent.Sequence.serializer()),
        Variant("action.local.set", ActionIntent.LocalSet::class, ActionIntent.LocalSet.serializer()),
        Variant("action.link.open", ActionIntent.OpenLink::class, ActionIntent.OpenLink.serializer()),
        Variant("action.device", ActionIntent.Device::class, ActionIntent.Device.serializer()),
        Variant("action.confirm", ActionIntent.Confirm::class, ActionIntent.Confirm.serializer()),
    )

    override fun unknown(tag: String, raw: JsonObject): ActionIntent = ActionIntent.Unknown(tag, raw)
    override fun unknownRaw(value: ActionIntent): JsonObject? = (value as? ActionIntent.Unknown)?.raw
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

object ConstructJson {
    val json: Json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false // omit null optionals (D3), matching serde skip_serializing_if
    }

    fun decodeBundle(s: String): WorkspaceSpec = json.decodeFromString(WorkspaceSpec.serializer(), s)
    fun encodeBundle(spec: WorkspaceSpec): String = json.encodeToString(WorkspaceSpec.serializer(), spec)

    fun decodeNode(s: String): UiNode = json.decodeFromString(UiNode.serializer(), s)
    fun encodeNode(node: UiNode): String = json.encodeToString(UiNode.serializer(), node)

    fun decodeEntities(s: String): Map<String, JsonObject> = json.decodeFromString(s)

    /** Produces e.g. `{"intent":"action.mutation.optimistic","entity_uri":...}`, accepted by the Rust serde enum. */
    fun encodeIntent(i: ActionIntent): String = json.encodeToString(ActionIntent.serializer(), i)
    fun decodeIntent(s: String): ActionIntent = json.decodeFromString(ActionIntent.serializer(), s)
}
