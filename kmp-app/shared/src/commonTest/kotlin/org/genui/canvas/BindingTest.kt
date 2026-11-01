package org.genui.canvas

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

    private fun pixelfedActionBar(): UiNode.ActionBar {
        val ws = CanvasJson.decodeBundle(PIXELFED_BUNDLE_JSON).workspace as UiNode.Workspace
        val card = (ws.slots[0].content as UiNode.FeedList).itemTemplate as UiNode.PostCard
        return card.actionSlot as UiNode.ActionBar
    }

    @Test
    fun bindIntentSubstitutesItemFieldsInMutation() {
        val bound = bindIntent(pixelfedActionBar().likeIntent, post)
        assertIs<ActionIntent.OptimisticMutate>(bound)
        assertEquals("pixelfed://posts/101", bound.entityUri)
        assertEquals("101", bound.payload.jsonObject["id"]!!.jsonPrimitive.content)
        assertEquals("!entity.is_liked", bound.valueExpr) // evaluated by the engine, not bound
        assertEquals("/api/v1/posts/like", bound.endpoint)
    }

    @Test
    fun bindIntentSubstitutesEmbeddedTokenInNavigate() {
        val bound = bindIntent(pixelfedActionBar().commentIntent, post)
        assertIs<ActionIntent.Navigate>(bound)
        assertEquals("pixelfed://posts/101/comments", bound.targetUri)
        assertEquals(PresentationMode.Modal, bound.presentation)
    }

    @Test
    fun bindIntentWithoutItemIsIdentity() {
        val intent = pixelfedActionBar().likeIntent
        assertSame(intent, bindIntent(intent, null))
    }

    @Test
    fun bindIntentBindsUnknownRawButKeepsTag() {
        val intent = CanvasJson.decodeIntent("""{"intent":"action.future","target":"${'$'}item.uri","n":3}""")
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
    }

    @Test
    fun resolveMediaRefsHandlesExprAndItems() {
        assertEquals(
            listOf("pixelfed://media/501", "pixelfed://media/502"),
            resolveMediaRefs(MediaRefs.Expr("${'$'}item.media_uris"), post),
        )
        assertEquals(listOf("a", "b"), resolveMediaRefs(MediaRefs.Items(listOf("a", "b")), null))
        assertEquals(emptyList(), resolveMediaRefs(MediaRefs.Expr("${'$'}item.missing"), post))
    }
}
