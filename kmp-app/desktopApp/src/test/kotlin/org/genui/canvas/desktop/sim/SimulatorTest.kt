package org.genui.canvas.desktop.sim

import kotlinx.serialization.json.JsonPrimitive
import org.genui.canvas.CanvasController
import java.awt.image.BufferedImage
import java.io.File
import java.nio.file.Files
import javax.imageio.ImageIO
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/** Smoke tests for simulation mode: the real CanvasApp, rendered offscreen, over [FakeEngineBridge]. */
class SimulatorTest {
    private val outDir: File = Files.createTempDirectory("canvas-sim-test").toFile()

    @AfterTest
    fun tearDown() {
        outDir.deleteRecursively()
    }

    private fun bundle(name: String): String = assertNotNull(findFixture(name), "fixtures/$name not found").readText()

    private fun run(fake: FakeEngineBridge, script: String, screen: String = "phone"): SimResult =
        SimRunner(
            controller = CanvasController(fake),
            outDir = outDir,
            screen = ScreenSpec.parse(screen),
            setOnline = fake::setOnline,
        ).run(parseScript(script))

    private fun image(result: SimResult, index: Int): BufferedImage {
        val file = assertNotNull(result.steps.single { it.index == index }.file, "step $index wrote no file")
        return ImageIO.read(file)
    }

    private fun assertNotBlank(img: BufferedImage) {
        val first = img.getRGB(0, 0)
        var differing = 0
        for (y in 0 until img.height step 4) for (x in 0 until img.width step 4) {
            if (img.getRGB(x, y) != first) differing++
        }
        assertTrue(differing > 100, "image looks blank ($differing differing samples)")
    }

    @Test
    fun homeFixtureOnPhone() {
        val fake = FakeEngineBridge(bundle("counter_home_bundle.json"))
        val result = run(
            fake,
            "shot initial; click Increment; click Increment; assert-text Count: 2; offline; assert-text Offline; " +
                "assert-no-text Online; shot offline",
        )
        assertTrue(result.ok, result.steps.joinToString("\n") { it.logLine() })

        val initial = image(result, 1)
        assertEquals(390 * 3, initial.width)
        assertEquals(844 * 3, initial.height)
        assertNotBlank(initial)
        assertNotBlank(image(result, 8))
        assertEquals(JsonPrimitive(2), fake.entityField("core://counters/home", "count"))

        assertTrue(File(outDir, "01-initial.png").isFile)
        assertTrue(File(outDir, "steps.log").readText().contains("RESULT: ok"))
        assertTrue(File(outDir, "steps.json").readText().contains("\"status\": \"ok\""))
    }

    @Test
    fun pixelfedFixtureRendersFixtureText() {
        val fake = FakeEngineBridge(bundle("pixelfed_home_bundle.json"))
        val result = run(fake, "assert-text Sunrise over Boulder; assert-text @aidan; shot feed", screen = "desktop")
        assertTrue(result.ok, result.steps.joinToString("\n") { it.logLine() })
        val img = image(result, 3)
        assertEquals(2560, img.width)
        assertEquals(1600, img.height)
        assertNotBlank(img)
    }

    @Test
    fun failingStepStopsTheRun() {
        val fake = FakeEngineBridge(bundle("counter_home_bundle.json"))
        val result = run(fake, "assert-text Count: 5; shot never")
        assertFalse(result.ok)
        assertEquals(1, result.steps.size)
        assertTrue(result.steps[0].message.contains("Count: 0"), result.steps[0].message)
        assertFalse(File(outDir, "02-never.png").exists())
        assertEquals(File(outDir, "01-FAILED.png"), result.steps[0].file)
        assertNotBlank(ImageIO.read(File(outDir, "01-FAILED.png")))
        assertTrue(File(outDir, "steps.json").readText().contains("\"file\": \"01-FAILED.png\""))
        assertTrue(File(outDir, "steps.log").readText().contains("RESULT: FAILED"))
    }

    @Test
    fun connectivityStepsAreRejectedBeforeRunningInLiveMode() {
        val steps = parseScript("shot a; offline")
        validateStepsForMode(steps, "fixture")
        val e = assertFailsWith<ScriptParseException> { validateStepsForMode(steps, "live") }
        assertTrue(e.message!!.contains("step 2 'offline' is not supported in live mode"), e.message)
    }
}
