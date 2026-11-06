package org.theconstruct.desktop.sim

import org.theconstruct.ConstructController
import java.io.File
import java.nio.file.Files
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * Each shell blueprint as the plumbing a pod serves this build (`blueprint_<id>_page.json`, a workspace expanded
 * through `core:workspace`, decision `plumbing-and-porcelain`) draws whole: its title, and nothing this build can't
 * draw. The app has no workspace drawing of its own (issue #126).
 */
class BlueprintPagesTest {
    private val outDir: File = Files.createTempDirectory("construct-blueprint-test").toFile()

    @AfterTest
    fun tearDown() {
        outDir.deleteRecursively()
    }

    private fun fixture(name: String): String = assertNotNull(findFixture(name), "${fixturePath(name)} not found").readText()

    @Test
    fun shellBlueprintsDrawAsPages() {
        for ((id, title) in listOf("shell" to "Notifications", "settings" to "Settings", "inbox" to "Notifications")) {
            val bundle = """{"version": "1.0.0", "workspace": ${fixture("blueprint_${id}_page.json")}, "entities": {}, "queries": {}}"""
            val bridge = FakeEngineBridge(bundle, appId = id, layoutBundles = { null })
            val runner = SimRunner(ConstructController(bridge, id), outDir, ScreenSpec.parse("phone"))
            val r = runner.run(
                parseScript(
                    "wait 300; assert-text $title; assert-no-text Unsupported component; " +
                        "assert-no-text This page needs a newer pod.; shot $id",
                ),
            )
            assertTrue(r.ok, r.steps.joinToString("\n") { it.logLine() })
        }
    }
}
