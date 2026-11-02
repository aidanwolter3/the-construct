package org.genui.canvas.desktop.sim

import org.genui.canvas.ActiveLayout
import org.genui.canvas.CanvasController
import org.genui.canvas.CanvasEngineBridge
import org.genui.canvas.LayoutApiException
import java.io.File
import java.nio.file.Files
import java.util.concurrent.atomic.AtomicInteger
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/** The layout browser (D30) against the fixture-mode fake engine, driven by the simulator's layout commands. */
class LayoutBrowserTest {
    private val outDir: File = Files.createTempDirectory("canvas-layouts-test").toFile()

    @AfterTest
    fun tearDown() {
        outDir.deleteRecursively()
    }

    private fun feed() = FakeEngineBridge(assertNotNull(findFixture("pixelfed_home_feed_bundle.json")).readText())

    /** Counts dispatched intents; optionally fails writes like a pod without a session. */
    private class Spy(val fake: FakeEngineBridge, val failWrites: Boolean = false) : CanvasEngineBridge by fake {
        val dispatches = AtomicInteger()

        override fun dispatch(intentJson: String) {
            dispatches.incrementAndGet()
            fake.dispatch(intentJson)
        }

        override fun activateLayout(layoutId: String, version: Int?): String =
            if (failWrites) throw LayoutApiException("Sign in to change layouts.", LayoutApiException.Kind.Unauthorized, 401)
            else fake.activateLayout(layoutId, version)
    }

    private fun run(bridge: CanvasEngineBridge, fake: FakeEngineBridge, script: String, expectOk: Boolean = true): Pair<SimResult, CanvasController> {
        val controller = CanvasController(bridge)
        val r = SimRunner(controller, outDir, ScreenSpec.parse("phone"), setOnline = fake::setOnline).run(parseScript(script))
        if (expectOk) assertTrue(r.ok, r.steps.joinToString("\n") { it.logLine() })
        return r to controller
    }

    @Test
    fun switchKeepsTheCarouselPageAndUndoGoesBack() {
        val fake = feed()
        val (r, controller) = run(
            fake, fake,
            "swipe-left tag:carousel:; assert-text Page 2 of 3; open-layouts; select-layout editorial; " +
                "assert-text Large portrait photo first; use-layout; assert-text Switch to Editorial journal?; confirm; " +
                "assert-text Switched to Editorial journal; assert-no-text Choose how this screen looks; " +
                // Same post, MediaCarousel moved to another slot: still on page 2.
                "assert-text Page 2 of 3; undo; assert-text Undone. Back to Classic feed; assert-text Page 2 of 3",
        )
        assertEquals("snackbar: Switched to Editorial journal Undo", r.steps.first { it.command == "confirm" }.message)
        assertEquals(ActiveLayout("pixelfed_home.classic", 4, 3), controller.state.value.activeLayout)
    }

    @Test
    fun restoreAnOlderVersionAndUndoIsAppendOnly() {
        val fake = feed()
        val (_, controller) = run(
            fake, fake,
            "assert-text Tram lines, Lisbon.; open-layouts; select-layout classic; assert-text Hide post captions; " +
                "select-version 2; restore-version; assert-text Restore v2?; confirm; assert-text Restored v2 as v5; " +
                "assert-no-text Tram lines, Lisbon.; undo; assert-text Undone. Restored v4 as v6; assert-text Tram lines, Lisbon.; " +
                "open-layouts; select-layout classic; assert-text 6 versions; select-version 6; assert-text This version is on screen now.",
        )
        assertEquals(ActiveLayout("pixelfed_home.classic", 6, 3), controller.state.value.activeLayout)
    }

    @Test
    fun previewsNeverDispatchAndTheBrowserBlocksTheFeed() {
        val fake = feed()
        val spy = Spy(fake)
        run(
            spy, fake,
            // A click on a thumbnail opens the details (the card), never the post inside it.
            "open-layouts; click tag:layout-item-pixelfed_home.compact; assert-text Use this layout; " +
                "select-version 1; click tag:version-preview; click Preview of Compact v1; " +
                // The feed's Like under the open browser: the click lands on the browser.
                "click Like; back; back; back; assert-text 27",
        )
        assertEquals(0, spy.dispatches.get())
        assertEquals("false", fake.entityField("pixelfed://posts/781254316723458049", "is_liked").toString())
    }

    @Test
    fun backClosesTheInnermostLevel() {
        val fake = feed()
        val (r, _) = run(
            fake, fake,
            "open-layouts; select-layout editorial; select-version 1; use-layout; back; assert-no-text Switch to Editorial journal?; " +
                "assert-text Version 1; back; assert-text Version history; back; assert-text Choose how this screen looks; " +
                "back; assert-no-text Choose how this screen looks; back",
            expectOk = false,
        )
        // The last Back has nothing to close.
        val last = r.steps.last()
        assertFalse(last.ok)
        assertTrue(last.message.contains("nothing on screen handles Back"), last.message)
        assertTrue(r.steps.dropLast(1).all { it.ok }, r.steps.joinToString("\n") { it.logLine() })
    }

    @Test
    fun offlineShowsAReadableErrorWithRetry() {
        val fake = feed()
        val (r, _) = run(
            fake, fake,
            "offline; open-layouts; assert-text Can't reach your pod.; online; click tag:layouts-retry; " +
                "assert-text Editorial journal; select-layout minimal",
        )
        assertTrue(r.steps[1].message.startsWith("error shown: Can't reach your pod."), r.steps[1].message)
    }

    @Test
    fun aRejectedWriteKeepsTheDialogWithTheReason() {
        val fake = feed()
        val (r, controller) = run(
            Spy(fake, failWrites = true), fake,
            "open-layouts; select-layout editorial; use-layout; confirm; assert-text Sign in to change layouts.; " +
                "assert-text Try again; cancel; assert-text Use this layout",
        )
        assertEquals("error shown: Sign in to change layouts.", r.steps.first { it.command == "confirm" }.message)
        assertEquals("pixelfed_home.classic", controller.state.value.activeLayout.layoutId)
    }

    @Test
    fun theOpenLevelAndUndoSurviveANewComposition() {
        val fake = feed()
        // `screen` disposes the composition and starts a new one on the same controller, like an Android rotation.
        val (_, controller) = run(
            fake, fake,
            "open-layouts; select-layout editorial; screen tablet; assert-text Use this layout; use-layout; confirm; " +
                "screen phone; assert-text Switched to Editorial journal; undo; assert-text Undone. Back to Classic feed",
        )
        assertEquals("pixelfed_home.classic", controller.state.value.activeLayout.layoutId)
    }

    @Test
    fun theAgentPromptSurvivesASwitch() {
        val fake = feed()
        run(
            fake, fake,
            "click tag:agent-fab; type tag:agent-prompt Make it calmer; click tag:agent-close; open-layouts; " +
                "select-layout minimal; use-layout; confirm; click tag:agent-fab; assert-text Make it calmer",
        )
    }

    @Test
    fun commandsFailClearlyWhenTheirTargetIsMissing() {
        val fake = feed()
        val (r, _) = run(fake, fake, "select-version 2", expectOk = false)
        assertTrue(r.steps[0].message.startsWith("no layout details on screen (use select-layout first)"), r.steps[0].message)
        val (r2, _) = run(fake, fake, "open-layouts; select-layout nope", expectOk = false)
        assertTrue(r2.steps[1].message.startsWith("no layout 'nope' in the browser; listed: [pixelfed_home.classic"), r2.steps[1].message)
        val (r3, _) = run(fake, fake, "open-layouts; select-layout classic; use-layout", expectOk = false)
        assertTrue(r3.steps[2].message.contains("for the active layout use restore-version"), r3.steps[2].message)
        val (r4, _) = run(fake, fake, "undo", expectOk = false)
        assertTrue(r4.steps[0].message.startsWith("no Undo on screen"), r4.steps[0].message)
    }

    @Test
    fun layoutCommandsParse() {
        val steps = parseScript("open-layouts; select-layout pixelfed_demo.editorial; select-version v3; use-layout; restore-version; confirm; cancel; undo; close-layouts; back")
        assertEquals(
            listOf("open-layouts", "select-layout pixelfed_demo.editorial", "select-version 3", "use-layout", "restore-version", "confirm", "cancel", "undo", "close-layouts", "back"),
            steps.map { it.toString() },
        )
        assertFailsWith<ScriptParseException> { parseScript("select-version 0") }
        assertFailsWith<ScriptParseException> { parseScript("select-version two") }
        assertFailsWith<ScriptParseException> { parseScript("select-layout") }
        assertFailsWith<ScriptParseException> { parseScript("confirm now") }
    }
}
