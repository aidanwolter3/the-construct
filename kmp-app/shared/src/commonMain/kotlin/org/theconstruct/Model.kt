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

// Kotlin mirror of crates/protocol (ast.rs, intent.rs, bundle.rs) and DESIGN.md §2.2.
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

@Serializable
data class SlotSpec(
    @SerialName("slot_id") val slotId: String,
    @SerialName("provider_uri") val providerUri: String,
    val content: UiNode,
)

@Serializable
enum class Orientation { Vertical, Horizontal }

@Serializable
enum class TextStyle { Headline, Title, Body, Caption, Label, Secondary }

/** `core://Chip` colour. */
@Serializable
enum class Tone { Neutral, Warning }

@Serializable
enum class PresentationMode { Push, Modal, Replace }

@Serializable
enum class ImageShape { Square, Rounded, Circle }

/**
 * A screen an app can open (DECISIONS D34; protocol `ast::Screen`). [route] is a URI pattern with `{name}` segments;
 * `$route.<name>` in the other fields is replaced by the captured value. [data] is a pod path loaded when the screen
 * opens; [footer] stays at the bottom while [content] scrolls.
 */
@Serializable
data class Screen(
    val id: String,
    val route: String,
    val title: String,
    val data: String? = null,
    val content: UiNode,
    val footer: UiNode? = null,
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

/** `core://List` group headers (schema 5, D43): [header] is drawn before each drawn item whose [field] changes. */
@Serializable
data class GroupBy(val field: String, val header: UiNode)

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
    ) : UiNode()

    @Serializable
    data class Container(
        val orientation: Orientation,
        @SerialName("padding_dp") val paddingDp: Int,
        val children: List<UiNode>,
    ) : UiNode()

    @Serializable
    data class Text(
        @SerialName("value_expr") val valueExpr: String,
        val style: TextStyle,
    ) : UiNode()

    @Serializable
    data class Button(
        val label: String,
        @SerialName("on_click") val onClick: ActionIntent,
    ) : UiNode()

    /**
     * `core://List` (D34): the entities of `queries[queryKey]`, each rendered with [itemTemplate]. [filter] and
     * [groupBy] are schema 5 (D43).
     */
    @Serializable
    data class ItemList(
        @SerialName("query_key") val queryKey: String,
        @SerialName("item_template") val itemTemplate: UiNode,
        val indent: Indent? = null,
        @SerialName("empty_text") val emptyText: String? = null,
        val filter: ListFilter? = null,
        @SerialName("group_by") val groupBy: GroupBy? = null,
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
    ) : UiNode()

    // Schema 3 (DECISIONS D36): generic building blocks for composing any app on the pod.

    /** Children side by side, vertically centred, not wrapping; a [Spacer] without a size takes the rest. */
    @Serializable
    data class Row(
        val children: List<UiNode>,
        @SerialName("spacing_dp") val spacingDp: Int = 0,
        @SerialName("padding_h_dp") val paddingHDp: Int = 0,
        @SerialName("padding_v_dp") val paddingVDp: Int = 0,
    ) : UiNode()

    /** Children stacked, full width. */
    @Serializable
    data class Column(
        val children: List<UiNode>,
        @SerialName("spacing_dp") val spacingDp: Int = 0,
        @SerialName("padding_h_dp") val paddingHDp: Int = 0,
        @SerialName("padding_v_dp") val paddingVDp: Int = 0,
    ) : UiNode()

    /** Empty space: [sizeDp], or (null, in a [Row]) the remaining width. */
    @Serializable
    data class Spacer(@SerialName("size_dp") val sizeDp: Int? = null) : UiNode()

    /** A thin horizontal line. */
    @Serializable
    data object Divider : UiNode()

    /** [content] only when [value] (e.g. `$item.caption`) is truthy ([isTruthy]). */
    @Serializable
    data class Conditional(val value: String, val content: UiNode) : UiNode()

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
        @SerialName("target_uri") val targetUri: String,
        val presentation: PresentationMode,
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

    /** `action.data.load` (D34): the engine loads a pod path (a screen's `data`) and caches its answer. */
    @Serializable
    data class LoadData(val path: String) : ActionIntent()

    /**
     * `action.mutation.create` (D34): the engine lists [entity] at [entityUri] in `queries[queryKey]` at once and sends
     * [payload] to [endpoint] through the outbox.
     */
    @Serializable
    data class Create(
        @SerialName("query_key") val queryKey: String,
        @SerialName("entity_uri") val entityUri: String,
        val entity: JsonElement,
        val endpoint: String,
        val payload: JsonElement,
        @SerialName("refresh_workspace") val refreshWorkspace: Boolean = false,
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
        Variant("action.data.load", ActionIntent.LoadData::class, ActionIntent.LoadData.serializer()),
        Variant("action.mutation.create", ActionIntent.Create::class, ActionIntent.Create.serializer()),
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
