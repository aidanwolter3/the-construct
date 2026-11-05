package org.theconstruct.desktop.sim

import org.theconstruct.ConstructController
import java.io.File
import java.nio.file.Files
import javax.imageio.ImageIO
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * Golden screenshots (decision `plumbing-and-porcelain`): each shell blueprint drawn as the workspace an older app gets
 * (`blueprint_<id>.json`) and as the plumbing this build gets (`blueprint_<id>_page.json`, expanded through
 * `core:workspace`) looks the same.
 */
class PlumbingGoldenTest {
    private val outDir: File = Files.createTempDirectory("construct-golden-test").toFile()

    @AfterTest
    fun tearDown() {
        outDir.deleteRecursively()
    }

    private fun fixture(name: String): String = assertNotNull(findFixture(name), "${fixturePath(name)} not found").readText()

    /** A phone screenshot of layout [layoutJson] drawn as the root. */
    private fun shot(id: String, layoutJson: String, name: String): java.awt.image.BufferedImage {
        val bundle = """{"version": "1.0.0", "workspace": $layoutJson, "entities": {}, "queries": {}}"""
        val bridge = FakeEngineBridge(bundle, appId = id, layoutBundles = { null })
        val runner = SimRunner(ConstructController(bridge, id), outDir, ScreenSpec.parse("phone"))
        val r = runner.run(parseScript("wait 300; shot $name"))
        assertTrue(r.ok, r.steps.joinToString("\n") { it.logLine() })
        return ImageIO.read(outDir.listFiles()!!.first { it.name.endsWith("$name.png") })
    }

    @Test
    fun shellBlueprintsLookTheSameAsWorkspaceAndAsPlumbing() {
        for (id in listOf("shell", "settings", "inbox")) {
            val a = shot(id, fixture("blueprint_$id.json"), "$id-workspace")
            val b = shot(id, fixture("blueprint_${id}_page.json"), "$id-page")
            assertTrue(a.width == b.width && a.height == b.height, "$id: sizes differ")
            var differ = 0
            for (y in 0 until a.height) for (x in 0 until a.width) if (a.getRGB(x, y) != b.getRGB(x, y)) differ++
            assertTrue(differ == 0, "$id: $differ of ${a.width * a.height} pixels differ")
        }
    }
}
