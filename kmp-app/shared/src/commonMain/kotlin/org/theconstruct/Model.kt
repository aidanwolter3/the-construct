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
enum class TextStyle { Headline, Title, Body, Caption }

@Serializable
enum class PresentationMode { Push, Modal, Replace }

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

    @Serializable
    data class PostCard(
        @SerialName("post_ref") val postRef: String,
        @SerialName("author_slot") val authorSlot: UiNode,
        @SerialName("media_slot") val mediaSlot: UiNode,
        @SerialName("action_slot") val actionSlot: UiNode,
        @SerialName("caption_slot") val captionSlot: UiNode,
    ) : UiNode()

    @Serializable
    data class MediaCarousel(
        @SerialName("media_refs") val mediaRefs: MediaRefs,
        @SerialName("aspect_ratio") val aspectRatio: Float,
    ) : UiNode()

    @Serializable
    data class AuthorHeader(
        @SerialName("user_ref") val userRef: String,
        // DECISIONS D3: omitted when null, defaults to null when missing.
        @SerialName("on_click") val onClick: ActionIntent? = null,
    ) : UiNode()

    @Serializable
    data class ActionBar(
        @SerialName("like_intent") val likeIntent: ActionIntent,
        @SerialName("bookmark_intent") val bookmarkIntent: ActionIntent,
        @SerialName("comment_intent") val commentIntent: ActionIntent,
    ) : UiNode()

    @Serializable
    data class FeedList(
        @SerialName("query_key") val queryKey: String,
        @SerialName("item_template") val itemTemplate: UiNode,
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

    @Serializable
    data class AuthLogin(
        @SerialName("instance_url") val instanceUrl: String,
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
// MediaRefs (DECISIONS D1: protocol::ListBinding, untagged)
// ---------------------------------------------------------------------------

@Serializable(with = MediaRefsSerializer::class)
sealed class MediaRefs {
    /** A binding expression such as `"$item.media_uris"`. */
    data class Expr(val expr: String) : MediaRefs()

    /** A literal list of entity URIs. */
    data class Items(val items: List<String>) : MediaRefs()
}

object MediaRefsSerializer : KSerializer<MediaRefs> {
    override val descriptor: SerialDescriptor = buildClassSerialDescriptor("org.theconstruct.MediaRefs")

    override fun deserialize(decoder: Decoder): MediaRefs {
        val input = decoder as? JsonDecoder ?: throw SerializationException("MediaRefs supports JSON only")
        val e = input.decodeJsonElement()
        return when {
            e is JsonPrimitive && e.isString -> MediaRefs.Expr(e.content)
            e is JsonArray -> MediaRefs.Items(
                e.map { (it as? JsonPrimitive)?.takeIf { p -> p.isString }?.content
                    ?: throw SerializationException("media_refs array must contain strings, got $it") }
            )
            else -> throw SerializationException("media_refs must be a string or an array of strings, got $e")
        }
    }

    override fun serialize(encoder: Encoder, value: MediaRefs) {
        val output = encoder as? JsonEncoder ?: throw SerializationException("MediaRefs supports JSON only")
        output.encodeJsonElement(
            when (value) {
                is MediaRefs.Expr -> JsonPrimitive(value.expr)
                is MediaRefs.Items -> JsonArray(value.items.map(::JsonPrimitive))
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
        Variant("pixelfed://PostCard", UiNode.PostCard::class, UiNode.PostCard.serializer()),
        Variant("pixelfed://MediaCarousel", UiNode.MediaCarousel::class, UiNode.MediaCarousel.serializer()),
        Variant("pixelfed://AuthorHeader", UiNode.AuthorHeader::class, UiNode.AuthorHeader.serializer()),
        Variant("pixelfed://ActionBar", UiNode.ActionBar::class, UiNode.ActionBar.serializer()),
        Variant("pixelfed://FeedList", UiNode.FeedList::class, UiNode.FeedList.serializer()),
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
