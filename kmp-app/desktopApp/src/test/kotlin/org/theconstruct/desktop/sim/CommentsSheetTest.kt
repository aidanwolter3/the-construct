package org.theconstruct.desktop.sim

import kotlinx.serialization.json.JsonPrimitive
import org.theconstruct.ConstructController
import java.io.File
import java.nio.file.Files
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/** The comments sheet (DECISIONS D34) on the `pixelfed_home` fixture with the fake upstream's comment threads. */
class CommentsSheetTest {
    private val outDir: File = Files.createTempDirectory("construct-comments-test").toFile()

    @AfterTest
    fun tearDown() {
        outDir.deleteRecursively()
    }

    private fun fake() = FakeEngineBridge(
        assertNotNull(findFixture("pixelfed_home_feed_bundle.json")).readText(),
        authJson = """{"state":"signed_in","username":"constructdemo"}""",
        commentsJson = assertNotNull(findFixture("pixelfed/comments.json")).readText(),
    )

    private fun run(fake: FakeEngineBridge, script: String): SimResult {
        val r = SimRunner(ConstructController(fake), outDir, ScreenSpec.parse("phone"), setOnline = fake::setOnline)
            .run(parseScript(script))
        assertTrue(r.ok, r.steps.joinToString("\n") { it.logLine() })
        return r
    }

    @Test
    fun commentButtonOpensTheThreadAndBackClosesIt() {
        run(
            fake(),
            "click tag:comment:$CAROUSEL; wait-text That second frame is unreal; assert-text Comments; " +
                "assert-text @bob thanks! Up at 5:40 for that one.; assert-text Which lens?; assert-text 3 likes; " +
                "back; assert-no-text Which lens?; assert-text Sunrise over Boulder",
        )
    }

    @Test
    fun postingACommentGoesThroughTheOutboxAndAppearsInTheThread() {
        val fake = fake()
        run(
            fake,
            "assert-text 4; click tag:comment:$CAROUSEL; wait-text Which lens?; " +
                "type tag:comment-input Golden light!; click tag:comment-send; wait-text Golden light!; " +
                "click tag:comments-close; assert-text 5",
        )
        assertEquals(JsonPrimitive(5), fake.entityField(CAROUSEL, "reply_count"))
        assertEquals(JsonPrimitive(5), fake.serverField(CAROUSEL, "reply_count"))
        assertTrue("Golden light!" in fake.commentsJson("781254316723458049"))
    }

    @Test
    fun offlineCommentsWaitThenSend() {
        val fake = fake()
        run(
            fake,
            "click tag:comment:$CAROUSEL; wait-text Which lens?; offline; " +
                "type tag:comment-input Sent from the tunnel; click tag:comment-send; " +
                "assert-text Waiting to send (offline); online; wait-text Sent from the tunnel; " +
                "assert-no-text Waiting to send",
        )
        assertEquals(0, fake.pendingActions)
        assertTrue("Sent from the tunnel" in fake.commentsJson("781254316723458049"))
    }

    @Test
    fun replyingToACommentNestsUnderIt() {
        val fake = fake()
        run(
            fake,
            "click tag:comment:$CAROUSEL; wait-text Which lens?; click tag:comment-reply:781300000000000104; " +
                "assert-text Replying to @carol; type tag:comment-input Same question; click tag:comment-send; " +
                "wait-text Same question; assert-no-text Replying to @carol",
        )
        // A reply to a comment does not change the post's (direct) comment count.
        assertEquals(JsonPrimitive(4), fake.entityField(CAROUSEL, "reply_count"))
        val thread = fake.commentsJson("781254316723458049")
        assertTrue(thread.indexOf("Same question") > thread.indexOf("24-70 at 35mm"), thread)
    }

    @Test
    fun aPostWithoutCommentsSaysSo() {
        val fake = FakeEngineBridge(
            assertNotNull(findFixture("pixelfed_home_feed_bundle.json")).readText(),
            commentsJson = null,
        )
        run(fake, "click tag:comment:$CAROUSEL; wait-text No comments yet")
    }

    companion object {
        const val CAROUSEL = "pixelfed://posts/781254316723458049"
    }
}
