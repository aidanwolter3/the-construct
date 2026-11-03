package org.theconstruct

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertSame

class BindingTest {
    private val post = JsonObject(
        mapOf(
            "uri" to JsonPrimitive("pixelfed://posts/101"),
            "id" to JsonPrimitive("101"),
            "author_uri" to JsonPrimitive("pixelfed://users/42"),
            "media_uris" to JsonArray(listOf(JsonPrimitive("pixelfed://media/501"), JsonPrimitive("pixelfed://media/502"))),
        )
    )

    /** The demo layout's icon button whose tag starts with [prefix] (e.g. "like:"). */
    private fun iconButton(prefix: String): UiNode.IconButton {
        fun all(n: UiNode): List<UiNode> = listOf(n) + when (n) {
            is UiNode.Workspace -> n.slots.flatMap { all(it.content) }
            is UiNode.Container -> n.children.flatMap(::all)
            is UiNode.Row -> n.children.flatMap(::all)
            is UiNode.Column -> n.children.flatMap(::all)
            is UiNode.ItemList -> all(n.itemTemplate)
            is UiNode.Carousel -> all(n.itemTemplate)
            is UiNode.Conditional -> all(n.content)
            is UiNode.Tappable -> all(n.content)
            else -> emptyList()
        }
        val ws = ConstructJson.decodeBundle(PIXELFED_BUNDLE_JSON).workspace
        return all(ws).filterIsInstance<UiNode.IconButton>().single { it.tag?.startsWith(prefix) == true }
    }

    @Test
    fun bindIntentSubstitutesItemFieldsInMutation() {
        val bound = bindIntent(iconButton("like:").onClick, post)
        assertIs<ActionIntent.OptimisticMutate>(bound)
        assertEquals("pixelfed://posts/101", bound.entityUri)
        assertEquals("101", bound.payload.jsonObject["id"]!!.jsonPrimitive.content)
        assertEquals("!entity.is_liked", bound.valueExpr) // evaluated by the engine, not bound
        assertEquals("/v1/demo/ack", bound.endpoint)
    }

    @Test
    fun bindIntentSubstitutesEmbeddedTokenInNavigate() {
        val bound = bindIntent(iconButton("comment:").onClick, post)
        assertIs<ActionIntent.Navigate>(bound)
        assertEquals("pixelfed://posts/101/comments", bound.targetUri)
        assertEquals(PresentationMode.Modal, bound.presentation)
    }

    @Test
    fun bindIntentWithoutItemIsIdentity() {
        val intent = iconButton("like:").onClick
        assertSame(intent, bindIntent(intent, null))
    }

    @Test
    fun bindIntentBindsUnknownRawButKeepsTag() {
        val intent = ConstructJson.decodeIntent("""{"intent":"action.future","target":"${'$'}item.uri","n":3}""")
        val bound = bindIntent(intent, post)
        assertIs<ActionIntent.Unknown>(bound)
        assertEquals("action.future", bound.raw["intent"]!!.jsonPrimitive.content)
        assertEquals("pixelfed://posts/101", bound.raw["target"]!!.jsonPrimitive.content)
        assertEquals(JsonPrimitive(3), bound.raw["n"])
    }

    @Test
    fun bindItemTokensLeavesMissingFieldsUntouched() {
        assertEquals("x/${'$'}item.nope/101", bindItemTokens("x/${'$'}item.nope/${'$'}item.id", post))
    }

    @Test
    fun resolveRefHandlesBindingsAndLiterals() {
        assertEquals("pixelfed://users/42", resolveRef("${'$'}item.author_uri", post))
        assertEquals("pixelfed://users/7", resolveRef("pixelfed://users/7", post))
        assertNull(resolveRef("${'$'}item.missing", post))
        assertNull(resolveRef("${'$'}item.author_uri", null))
        // Embedded tokens (DESIGN.md §2.2A: "entity://posts/$item.id") are substituted.
        assertEquals("pixelfed://posts/101", resolveRef("pixelfed://posts/${'$'}item.id", post))
    }

    @Test
    fun mutationWithNullRollbackRoundTripsAndKeepsField() {
        // Rust's `rollback_value: serde_json::Value` is required, so a null must be emitted.
        val json = """{"intent":"action.mutation.optimistic","entity_uri":"a","field":"f","value_expr":"0","rollback_value":null,"endpoint":"/e","payload":{}}"""
        val intent = ConstructJson.decodeIntent(json)
        assertIs<ActionIntent.OptimisticMutate>(intent)
        val encoded = ConstructJson.encodeIntent(intent)
        assertEquals(true, encoded.contains("\"rollback_value\":null"), encoded)
    }

    @Test
    fun resolveListHandlesExprAndItems() {
        assertEquals(
            listOf("pixelfed://media/501", "pixelfed://media/502"),
            resolveList(ListBinding.Expr("${'$'}item.media_uris"), post),
        )
        assertEquals(listOf("a", "b"), resolveList(ListBinding.Items(listOf("a", "b")), null))
        assertEquals(emptyList(), resolveList(ListBinding.Expr("${'$'}item.missing"), post))
    }

    @Test
    fun bindIntentKeepsAlsoAndValueTokenUntouched() {
        val d = '$'
        val intent = ActionIntent.OptimisticMutate(
            entityUri = "${d}item.uri",
            field = "is_liked",
            valueExpr = "!entity.is_liked",
            rollbackValue = JsonPrimitive(false),
            endpoint = "/v1/pixelfed/posts/like",
            payload = JsonObject(mapOf("id" to JsonPrimitive("${d}item.id"), "liked" to JsonPrimitive("${d}value"))),
            also = listOf(FieldUpdate("like_count", "entity.is_liked ? entity.like_count - 1 : entity.like_count + 1")),
        )
        val bound = assertIs<ActionIntent.OptimisticMutate>(bindIntent(intent, post))
        assertEquals("pixelfed://posts/101", bound.entityUri)
        assertEquals("101", bound.payload.jsonObject["id"]!!.jsonPrimitive.content)
        // "$value" is substituted by the engine with the primary field's new value, not by binding.
        assertEquals("${d}value", bound.payload.jsonObject["liked"]!!.jsonPrimitive.content)
        assertEquals(intent.also, bound.also)

        // The dispatch path (ConstructApp) re-encodes the bound intent: `also` must survive.
        val encoded = ConstructJson.json.parseToJsonElement(ConstructJson.encodeIntent(bound)).jsonObject
        val also = encoded["also"] as JsonArray
        assertEquals("like_count", also[0].jsonObject["field"]!!.jsonPrimitive.content)
        assertEquals(
            "entity.is_liked ? entity.like_count - 1 : entity.like_count + 1",
            also[0].jsonObject["value_expr"]!!.jsonPrimitive.content,
        )
        assertEquals("${d}value", encoded["payload"]!!.jsonObject["liked"]!!.jsonPrimitive.content)
    }
}
