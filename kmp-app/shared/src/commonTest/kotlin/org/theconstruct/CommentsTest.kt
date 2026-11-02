package org.theconstruct

import kotlin.random.Random
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

private class RecordingBridge : ConstructEngineBridge {
    val dispatched = mutableListOf<String>()
    var listener: ((EngineSnapshot) -> Unit)? = null
    override fun subscribe(onState: (EngineSnapshot) -> Unit) {
        listener = onState
    }
    override fun dispatch(intentJson: String) {
        dispatched += intentJson
    }
}

class CommentsTest {
    @Test
    fun parsesThePodsCommentsList() {
        val list = CommentsJson.parse(
            """{"post_id":"1","comments":[
              {"id":"10","in_reply_to_id":"1","depth":0,"author":{"id":"5","username":"bob","display_name":"Bob",
               "avatar_url":"http://pod/v1/pixelfed/media/ab"},"text":"Hi\nthere","created_at":1793500000,
               "like_count":3,"is_liked":true,"is_own":false},
              {"id":"11","in_reply_to_id":"10","depth":1,"author":{"username":"me","display_name":null},"text":"yo",
               "is_own":true},
              {"text":"no id: skipped"}, 42]}""",
        )
        assertEquals(2, list.size)
        val a = list[0]
        assertEquals(Comment("10", "1", 0, "bob", "Bob", "http://pod/v1/pixelfed/media/ab", "Hi\nthere", 1793500000, 3, true, false), a)
        val b = list[1]
        assertEquals(1, b.depth)
        assertNull(b.displayName)
        assertEquals(0, b.createdAt)
        assertTrue(b.isOwn)
        assertFailsWith<Exception> { CommentsJson.parse("""{"nope":[]}""") }
    }

    @Test
    fun onlyCommentTargetsAreRecognized() {
        assertEquals("781254316723458049", commentsTarget("pixelfed://posts/781254316723458049/comments"))
        assertEquals("abc_1-2", commentsTarget(" pixelfed://posts/abc_1-2/comments "))
        for (other in listOf(
            "pixelfed://posts/1", "pixelfed://bookmarks", "pixelfed://posts//comments",
            "pixelfed://posts/a/b/comments", "pixelfed://posts/\$item.id/comments", "core://posts/1/comments",
        )) {
            assertNull(commentsTarget(other), other)
        }
    }

    @Test
    fun commentIntentIsAnOutboxMutationOnReplyCount() {
        val intent = commentIntent("7", "Nice!", "abcdef0123456789abcdef01", replyCount = 4)
        val json = ConstructJson.json.parseToJsonElement(ConstructJson.encodeIntent(intent)).jsonObject
        assertEquals("action.mutation.optimistic", json["intent"]!!.jsonPrimitive.content)
        assertEquals("pixelfed://posts/7", json["entity_uri"]!!.jsonPrimitive.content)
        assertEquals("reply_count", json["field"]!!.jsonPrimitive.content)
        assertEquals("entity.reply_count + 1", json["value_expr"]!!.jsonPrimitive.content)
        assertEquals(JsonPrimitive(4), json["rollback_value"])
        assertEquals("/v1/pixelfed/posts/comment", json["endpoint"]!!.jsonPrimitive.content)
        val payload = json["payload"]!!.jsonObject
        assertEquals("7", payload["id"]!!.jsonPrimitive.content)
        assertEquals("Nice!", payload["text"]!!.jsonPrimitive.content)
        assertEquals("abcdef0123456789abcdef01", payload["client_id"]!!.jsonPrimitive.content)
        // A reply to a comment targets the comment and leaves the post's count alone.
        val reply = commentIntent("7", "Same", "x".repeat(24), replyCount = 4, replyToId = "99")
        assertEquals("entity.reply_count", reply.valueExpr)
        assertEquals("99", reply.payload.jsonObject["id"]!!.jsonPrimitive.content)
        assertEquals("pixelfed://posts/7", reply.entityUri)
    }

    @Test
    fun clientIdsAreHexNonces() {
        val id = newCommentClientId(Random(1))
        assertEquals(24, id.length)
        assertTrue(id.all { it in '0'..'9' || it in 'a'..'f' }, id)
        assertFalse(newCommentClientId() == newCommentClientId())
    }

    @Test
    fun draftRules() {
        assertNotNull(commentDraftProblem("   "))
        assertNotNull(commentDraftProblem("x".repeat(MAX_COMMENT_CHARS + 1)))
        assertNotNull(commentDraftProblem(" \$value "))
        assertNull(commentDraftProblem("  ok  "))
        assertNull(commentDraftProblem("x".repeat(MAX_COMMENT_CHARS)))
    }

    @Test
    fun commentNavigationOpensTheSheetInsteadOfReachingTheEngine() {
        val bridge = RecordingBridge()
        val controller = ConstructController(bridge)
        controller.dispatch(ActionIntent.Navigate("pixelfed://posts/42/comments", PresentationMode.Modal))
        assertEquals("42", controller.comments.postId)
        assertTrue(bridge.dispatched.isEmpty())
        // Other navigation still goes to the engine.
        controller.dispatch(ActionIntent.Navigate("pixelfed://bookmarks", PresentationMode.Push))
        assertEquals(1, bridge.dispatched.size)
        // Back: cancels a reply first, then closes.
        controller.comments.reply(Comment("9", username = "bob"))
        controller.dispatch(ActionIntent.NavigateBack)
        assertNull(controller.comments.replyTo)
        assertEquals("42", controller.comments.postId)
        controller.dispatch(ActionIntent.NavigateBack)
        assertNull(controller.comments.postId)
        assertEquals(1, bridge.dispatched.size)
        // With the sheet closed, Back is forwarded as before.
        controller.dispatch(ActionIntent.NavigateBack)
        assertEquals(2, bridge.dispatched.size)
    }

    @Test
    fun sendDispatchesTheOutboxIntentWithTheCurrentCount() {
        val bridge = RecordingBridge()
        val controller = ConstructController(bridge)
        bridge.listener!!(
            EngineSnapshot("null", """{"pixelfed://posts/42":{"id":"42","reply_count":3}}""", true, 1, 0),
        )
        val c = controller.comments
        controller.dispatch(ActionIntent.Navigate("pixelfed://posts/42/comments", PresentationMode.Modal))
        c.onDraftChange("   ")
        assertFalse(c.send())
        assertNotNull(c.draftError)
        assertTrue(bridge.dispatched.isEmpty())

        c.onDraftChange("  Lovely  ")
        assertNull(c.draftError)
        assertTrue(c.send())
        val sent = ConstructJson.decodeIntent(bridge.dispatched.single()) as ActionIntent.OptimisticMutate
        assertEquals(JsonPrimitive(3), sent.rollbackValue)
        assertEquals("Lovely", sent.payload.jsonObject["text"]!!.jsonPrimitive.content)
        assertEquals("", c.draft)
        assertEquals(listOf("Lovely"), c.pending.map { it.text })

        c.reply(Comment("77", username = "carol"))
        c.onDraftChange("re")
        assertTrue(c.send())
        val reply = ConstructJson.decodeIntent(bridge.dispatched.last()) as ActionIntent.OptimisticMutate
        assertEquals("77", reply.payload.jsonObject["id"]!!.jsonPrimitive.content)
        assertNull(c.replyTo)
        assertEquals("77", c.pending.last().replyTo?.id)

        // Opening another post starts fresh.
        controller.dispatch(ActionIntent.Navigate("pixelfed://posts/43/comments", PresentationMode.Modal))
        assertTrue(c.pending.isEmpty())
        assertEquals(Load.Loading, c.comments)
    }
}
