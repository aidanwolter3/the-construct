package org.genui.canvas.desktop.sim

import androidx.compose.ui.geometry.Size
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.getBoundsInRoot
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.test.performTextClearance
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.runSkikoComposeUiTest
import androidx.compose.ui.unit.Density
import androidx.compose.runtime.CompositionLocalProvider
import org.genui.canvas.ActionIntent
import org.genui.canvas.BackPresses
import org.genui.canvas.LocalBackPresses
import org.genui.canvas.Orientation
import org.genui.canvas.CanvasApp
import org.genui.canvas.CanvasController
import org.genui.canvas.CanvasEngineBridge
import org.genui.canvas.CanvasJson
import org.genui.canvas.EngineSnapshot
import org.genui.canvas.TextStyle
import org.genui.canvas.UiNode
import java.io.File
import java.nio.file.Files
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
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
                "wait-text 2000 I can't do that yet. Try: Hide captions; assert-text Make everything purple; " +
                "assert-text Tram lines, Lisbon.",
        )
        assertEquals(1, fake.layoutVersion)
    }

    @Test
    fun aPromptThatChangesNothingSaysNoChangeNeeded() {
        val fake = FakeEngineBridge(bundle())
        run(
            fake,
            "click tag:agent-fab; click Add a weather widget; click tag:agent-send; wait-text 2000 Layout updated (v2, fake-rules); " +
                "click Add a weather widget; click tag:agent-send; wait-text 2000 No change needed (v2); " +
                "assert-no-text Layout updated",
        )
        assertEquals(2, fake.layoutVersion)
    }

    @Test
    fun weatherPlaceholderIsPadded() {
        assertEquals(
            UiNode.Container(Orientation.Vertical, 16, listOf(UiNode.Text("Weather widget (not available yet)", TextStyle.Caption))),
            FakeLayoutRules.WEATHER_CONTENT,
        )
        val fake = FakeEngineBridge(bundle())
        fake.dispatch(CanvasJson.encodeIntent(ActionIntent.PromptAgent("add a weather widget")))
        SimImages.install("fixture")
        runSkikoComposeUiTest(size = Size(390f, 900f), density = Density(1f)) {
            setContent { CanvasApp(CanvasController(fake)) }
            waitForIdle()
            val text = onNode(hasText("Weather widget (not available yet)")).getBoundsInRoot()
            assertTrue(text.left.value >= 16f && text.top.value >= 16f, "weather text at $text should be inset by 16")
        }
    }

    @Test
    fun backClosesTheOpenPanelAndKeepsTheText() {
        val fake = FakeEngineBridge(bundle())
        val presses = BackPresses()
        SimImages.install("fixture")
        runSkikoComposeUiTest(size = Size(390f, 900f), density = Density(1f)) {
            setContent {
                CompositionLocalProvider(LocalBackPresses provides presses) { CanvasApp(CanvasController(fake)) }
            }
            waitForIdle()
            // Closed panel: Back is not consumed (Android then leaves the app as usual).
            assertFalse(presses.press())
            onNode(hasTestTag("agent-fab")).performClick()
            waitForIdle()
            onNode(hasTestTag("agent-prompt")).performTextInput("Photos first")
            waitForIdle()
            assertTrue(presses.press())
            waitForIdle()
            assertEquals(0, onAllNodes(hasTestTag("agent-panel")).fetchSemanticsNodes().size)
            assertFalse(presses.press(), "the handler is disabled once the panel is closed")
            onNode(hasTestTag("agent-fab")).performClick()
            waitForIdle()
            assertEquals("Photos first", promptText())
        }
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

    /**
     * A bridge whose agent status only changes when the test says so, like the real engine while the pod's LLM call
     * is in flight (the fake answers synchronously). It records the prompts it was sent.
     */
    private class ManualAgentBridge(bundleJson: String) : CanvasEngineBridge {
        private val workspaceJson = CanvasJson.encodeNode(CanvasJson.decodeBundle(bundleJson).workspace)
        private var listener: ((EngineSnapshot) -> Unit)? = null
        private var seq = 0L
        val prompts = mutableListOf<String>()

        override fun subscribe(onState: (EngineSnapshot) -> Unit) {
            listener = onState
            agent("{\"state\":\"idle\"}")
        }

        override fun dispatch(intentJson: String) {
            (CanvasJson.decodeIntent(intentJson) as? ActionIntent.PromptAgent)?.let { prompts += it.promptText }
        }

        fun agent(json: String) {
            listener?.invoke(EngineSnapshot(workspaceJson, "{}", true, ++seq, pendingActions = 0, agentJson = json))
        }
    }

    @Test
    fun aDoubleTapPostsThePromptOnce() {
        val bridge = ManualAgentBridge(bundle())
        val controller = CanvasController(bridge)
        SimImages.install("fixture")
        runSkikoComposeUiTest(size = Size(390f, 900f), density = Density(1f)) {
            setContent { CanvasApp(controller) }
            waitForIdle()
            onNode(hasTestTag("agent-fab")).performClick()
            waitForIdle()
            onNode(hasTestTag("agent-prompt")).performTextInput("Hide captions")
            waitForIdle()
            // Two taps before any recomposition (no snapshot yet: the engine has not reported running).
            mainClock.autoAdvance = false
            // (Semantics clicks run onClick synchronously, like a tap's handler; two in a row stand for a double
            // tap or the IME action plus the button landing within one frame.)
            onNode(hasTestTag("agent-send")).performSemanticsAction(SemanticsActions.OnClick)
            onNode(hasTestTag("agent-send")).performSemanticsAction(SemanticsActions.OnClick)
            mainClock.autoAdvance = true
            waitForIdle()
            assertEquals(listOf("Hide captions"), bridge.prompts)
            onNode(hasTestTag("agent-send")).assertIsNotEnabled()
            // A newer snapshot (here: an error) re-enables Send.
            bridge.agent("{\"state\":\"error\",\"message\":\"offline: try again when online\"}")
            waitForIdle()
            onNode(hasTestTag("agent-send")).performClick()
            waitForIdle()
            assertEquals(listOf("Hide captions", "Hide captions"), bridge.prompts)
        }
    }

    @Test
    fun anEditWhileRunningIsNotClearedByTheResult() {
        val bridge = ManualAgentBridge(bundle())
        val controller = CanvasController(bridge)
        SimImages.install("fixture")
        runSkikoComposeUiTest(size = Size(390f, 900f), density = Density(1f)) {
            setContent { CanvasApp(controller) }
            waitForIdle()
            onNode(hasTestTag("agent-fab")).performClick()
            waitForIdle()
            onNode(hasTestTag("agent-prompt")).performTextInput("Hide captions")
            waitForIdle()
            onNode(hasTestTag("agent-send")).performClick()
            bridge.agent("{\"state\":\"running\"}")
            waitForIdle()
            onNode(hasTestTag("agent-send")).assertIsNotEnabled()
            // The user starts the next prompt while the agent works.
            onNode(hasTestTag("agent-prompt")).performTextClearance()
            onNode(hasTestTag("agent-prompt")).performTextInput("Add a weather widget")
            bridge.agent("{\"state\":\"done\",\"provider\":\"rules\",\"layout_version\":2}")
            waitForIdle()
            assertEquals("Add a weather widget", promptText())
            onNode(hasTestTag("agent-status")).assertTextEquals("Layout updated (v2, rules)")
            // Unedited: the result clears the sent text.
            onNode(hasTestTag("agent-send")).performClick()
            bridge.agent("{\"state\":\"running\"}")
            bridge.agent("{\"state\":\"done\",\"provider\":\"rules\",\"layout_version\":3}")
            waitForIdle()
            assertEquals("", promptText())
            assertEquals(listOf("Hide captions", "Add a weather widget"), bridge.prompts)
        }
    }

    @Test
    fun fakeRulesMatchTheServerOnNoOpsAndLayoutsWithoutPostCards() {
        // The counter fixture has no PostCard: like the server's rules provider, caption/media rules fail there.
        val counter = CanvasJson.decodeBundle(assertNotNull(findFixture("counter_home_bundle.json")).readText()).workspace
        assertEquals(FakeLayoutRules.Outcome.Failed(FakeLayoutRules.NO_CARD), FakeLayoutRules.run("Hide captions", counter))
        // A repeated prompt leaves the layout as is and commits no new version.
        val fake = FakeEngineBridge(bundle())
        fake.dispatch(CanvasJson.encodeIntent(ActionIntent.PromptAgent("add a weather widget")))
        fake.dispatch(CanvasJson.encodeIntent(ActionIntent.PromptAgent("add a weather widget")))
        assertEquals(2, fake.layoutVersion)
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
