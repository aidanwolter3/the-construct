package org.theconstruct.desktop.sim

import org.theconstruct.ConstructController
import org.theconstruct.INBOX_LAYOUT
import org.theconstruct.LayoutEntry
import org.theconstruct.PresentationMode
import java.io.File
import java.nio.file.Files
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * Layouts presented over the root (decision `layouts-not-apps`): pushed, as a sheet and as a side panel, on one fake
 * engine, each popped by system Back.
 */
class LayoutsSimTest {
    private val outDir: File = Files.createTempDirectory("construct-layouts-test").toFile()

    @AfterTest
    fun tearDown() {
        outDir.deleteRecursively()
    }

    private fun fixture(name: String): String = assertNotNull(findFixture(name), "${fixturePath(name)} not found").readText()

    private fun controller(): Pair<ConstructController, FakeEngineBridge> {
        val bridge = FakeEngineBridge(
            fixture("pixelfed_feed_bundle.json"), appId = "pixelfed",
            layoutBundles = { id -> layoutBundle(id, File("").absoluteFile) },
        )
        return ConstructController(bridge, "pixelfed") to bridge
    }

    private fun SimRunner.ok(script: String): SimResult = run(parseScript(script)).also {
        assertTrue(it.ok, it.steps.joinToString("\n") { s -> s.logLine() })
    }

    @Test
    fun pushSheetAndPanelEachPopWithBack() {
        val (controller, _) = controller()
        val r = SimRunner(controller, outDir, ScreenSpec.parse("phone")).ok(
            "open-layout events push; assert-text Events; open-settings; assert-text Leave pod on this device; " +
                "open-inbox; back; back; back; assert-no-text Leave pod on this device",
        )
        assertEquals("events shown (push)", r.steps[0].message)
        assertEquals("settings shown (panel)", r.steps[2].message)
        assertNull(controller.navigation.top, "three Backs popped all three")
    }

    @Test
    fun aSheetDraggedPastAQuarterOfItsHeightCloses() {
        val (controller, _) = controller()
        val runner = SimRunner(controller, outDir, ScreenSpec.parse("phone"))
        // The phone's sheet is about 717 dp tall: 100 dp is short of a quarter and springs back.
        runner.ok("open-inbox; drag-down 100 tag:sheet-handle; idle")
        assertEquals(INBOX_LAYOUT, assertIs<LayoutEntry>(controller.navigation.top).layoutId, "a short drag springs back")
        runner.ok("drag-down 300 tag:sheet-handle; idle")
        assertNull(controller.navigation.top, "past a quarter closes it")
    }

    @Test
    fun aSheetFlungDownCloses() {
        val (controller, _) = controller()
        SimRunner(controller, outDir, ScreenSpec.parse("phone")).ok("open-inbox; swipe-down tag:sheet-handle; idle")
        assertNull(controller.navigation.top, "a fling closes it however short")
    }

    @Test
    fun aSheetPulledDownByItsContentAtTheTopCloses() {
        val (controller, _) = controller()
        val runner = SimRunner(controller, outDir, ScreenSpec.parse("phone"))
        // From the sheet's middle: its content, scrolled to the top.
        runner.ok("open-inbox; drag-down 100 tag:sheet:inbox; idle")
        assertEquals(INBOX_LAYOUT, assertIs<LayoutEntry>(controller.navigation.top).layoutId, "a short pull springs back")
        runner.ok("drag-down 300 tag:sheet:inbox; idle")
        assertNull(controller.navigation.top, "a long pull closes it")
    }

    @Test
    fun aMissingLayoutSaysSoAndBackStillWorks() {
        val (controller, _) = controller()
        val runner = SimRunner(controller, outDir, ScreenSpec.parse("phone"))
        val r = runner.run(parseScript("open-layout c-missing00000"))
        assertTrue(!r.ok && r.steps[0].message.contains("can't be shown"), r.steps[0].message)
        val entry = assertIs<LayoutEntry>(controller.navigation.top)
        assertEquals(PresentationMode.Push, entry.presentation)
        runner.ok("back")
        assertNull(controller.navigation.top)
    }
}
