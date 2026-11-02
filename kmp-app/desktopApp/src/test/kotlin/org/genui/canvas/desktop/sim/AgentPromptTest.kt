package org.genui.canvas.desktop.sim

import androidx.compose.ui.geometry.Size
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.getBoundsInRoot
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.runSkikoComposeUiTest
import androidx.compose.ui.unit.Density
import org.genui.canvas.ActionIntent
import org.genui.canvas.CanvasApp
import org.genui.canvas.CanvasController
import org.genui.canvas.CanvasJson
import org.genui.canvas.TextStyle
import org.genui.canvas.UiNode
import java.io.File
import java.nio.file.Files
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** The in-app layout agent (FAB + prompt panel) against the fixture-mode fake engine's rules subset (M7). */
@OptIn(ExperimentalTestApi::class)
class AgentPromptTest {
    private val outDir: File = Files.createTempDirectory("canvas-agent-test").toFile()

    @AfterTest
    fun tearDown() {
        outDir.deleteRecursively()
    }

    private fun bundle() = assertNotNull(findFixture("pixelfed_home_feed_bundle.json")).readText()

    private fun run(fake: FakeEngineBridge, script: String): SimResult {
        val r = SimRunner(CanvasController(fake), outDir, ScreenSpec.parse("phone"), setOnline = fake::setOnline)
            .run(parseScript(script))
        assertTrue(r.ok, r.steps.joinToString("\n") { it.logLine() })
        return r
    }

    @Test
    fun fabOpensThePanelAndChipsFillTheField() {
        run(
            FakeEngineBridge(bundle()),
            "assert-no-text Media at the very top; click Edit layout with AI; assert-text Media at the very top; " +
                "assert-text Add a weather widget; click Hide captions; assert-text Send; " +
                // The chip's text is now in the field; the close button hides the panel and the FAB is back.
                "click Close; assert-no-text Add a weather widget; click tag:agent-fab; assert-text Add a weather widget",
        )
    }

    @Test
    fun hideCaptionsPromptAppliesAndClearsTheField() {
        val fake = FakeEngineBridge(bundle())
        run(
            fake,
            "assert-text Tram lines, Lisbon.; click tag:agent-fab; type tag:agent-prompt Hide post captions; " +
                "click tag:agent-send; wait-text 2000 Layout updated (v2, fake-rules); assert-no-text Tram lines, Lisbon.; " +
                "assert-no-text Hide post captions",
        )
        assertEquals(2, fake.layoutVersion)
    }

    @Test
    fun unsupportedPromptShowsTheErrorAndKeepsTheText() {
        val fake = FakeEngineBridge(bundle())
        run(
            fake,
            "click tag:agent-fab; type tag:agent-prompt Make everything purple; click tag:agent-send; " +
                "wait-text 2000 unsupported prompt; assert-text Make everything purple; assert-text Tram lines, Lisbon.",
        )
        assertEquals(1, fake.layoutVersion)
    }

    @Test
    fun offlinePromptFailsWithoutChangingTheLayout() {
        run(
            FakeEngineBridge(bundle()),
            "offline; click tag:agent-fab; click Hide captions; click tag:agent-send; wait-text 2000 offline:; " +
                "assert-text Tram lines, Lisbon.",
        )
    }

    @Test
    fun carouselPageSurvivesLayoutChangesThatKeepThePost() {
        run(
            FakeEngineBridge(bundle()),
            "swipe-left tag:carousel:$CAROUSEL; assert-text Page 2 of 3; click tag:agent-fab; click Hide captions; " +
                "click tag:agent-send; wait-text 2000 Layout updated (v2; assert-no-text Tram lines, Lisbon.; " +
                "assert-text Page 2 of 3; " +
                // The carousel moves to another PostCard slot; its pager state moves with it (slots keyed by kind).
                "click Media at the very top; click tag:agent-send; wait-text 2000 Layout updated (v3; assert-text Page 2 of 3; " +
                // A slot inserted above the feed keeps the feed's state too (workspace slots keyed by slot id).
                "click Add a weather widget; click tag:agent-send; wait-text 2000 Layout updated (v4; " +
                "assert-text Weather widget (not available yet); assert-text Page 2 of 3",
        )
    }

    @Test
    fun promptTextAndPanelSurviveALayoutUpdateFromElsewhere() {
        val fake = FakeEngineBridge(bundle())
        val controller = CanvasController(fake)
        SimImages.install("fixture")
        runSkikoComposeUiTest(size = Size(390f, 1600f), density = Density(1f)) {
            setContent { CanvasApp(controller) }
            waitForIdle()
            onNode(hasTestTag("agent-fab")).performClick()
            waitForIdle()
            onNode(hasTestTag("agent-prompt")).performTextInput("Media at the very top")
            waitForIdle()
            // A layout change the panel did not send (e.g. another device's prompt arriving over SSE).
            fake.dispatch(CanvasJson.encodeIntent(ActionIntent.PromptAgent("hide captions")))
            waitForIdle()
            assertEquals(0, onAllNodes(hasText("Tram lines", substring = true)).fetchSemanticsNodes().size)
            assertEquals("Media at the very top", promptText())
            // Sending the typed prompt: media now renders above the author header, and the field is cleared.
            onNode(hasTestTag("agent-send")).performClick()
            waitForIdle()
            val media = onNode(hasTestTag("carousel:$CAROUSEL")).getBoundsInRoot().top.value
            val author = onAllNodes(hasText("@alice", substring = true))[0].getBoundsInRoot().top.value
            assertTrue(media < author, "media top $media should be above author $author")
            assertEquals("", promptText())
            onNode(hasTestTag("agent-send")).assertIsNotEnabled() // blank
        }
    }

    @Test
    fun fakeRulesAreComposableAndIdempotent() {
        val layout = CanvasJson.decodeBundle(bundle()).workspace
        assertNull(FakeLayoutRules.apply("make it purple", layout))
        val both = assertIs<UiNode.Workspace>(FakeLayoutRules.apply("Hide post captions and put media at the very top", layout))
        val card = assertIs<UiNode.PostCard>(assertIs<UiNode.FeedList>(both.slots[0].content).itemTemplate)
        assertEquals(UiNode.Text("", TextStyle.Caption), card.captionSlot)
        assertIs<UiNode.MediaCarousel>(card.authorSlot)
        assertIs<UiNode.AuthorHeader>(card.mediaSlot)
        // Applying "media at the top" again keeps media first; the weather slot is added once.
        val again = assertIs<UiNode.Workspace>(FakeLayoutRules.apply("photos first", both))
        assertIs<UiNode.MediaCarousel>(assertIs<UiNode.PostCard>(assertIs<UiNode.FeedList>(again.slots[0].content).itemTemplate).authorSlot)
        val weather = assertIs<UiNode.Workspace>(FakeLayoutRules.apply("Add a weather widget at the top", again))
        val twice = assertIs<UiNode.Workspace>(FakeLayoutRules.apply("add a weather widget", weather))
        assertEquals(listOf("weather", "feed"), twice.slots.map { it.slotId })
    }

    private fun androidx.compose.ui.test.SkikoComposeUiTest.promptText(): String =
        onNode(hasTestTag("agent-prompt")).fetchSemanticsNode().config
            .getOrNull(SemanticsProperties.EditableText)?.text.orEmpty()

    private companion object {
        const val CAROUSEL = "pixelfed://posts/781254316723458049"
    }
}
