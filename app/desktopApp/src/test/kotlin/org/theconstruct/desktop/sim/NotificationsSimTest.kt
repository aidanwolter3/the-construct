package org.theconstruct.desktop.sim

import androidx.compose.ui.geometry.Size
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.SkikoComposeUiTest
import androidx.compose.ui.test.hasParent
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.runSkikoComposeUiTest
import androidx.compose.ui.unit.Density
import org.theconstruct.AppNotification
import org.theconstruct.ConstructApp
import org.theconstruct.ConstructController
import java.io.File
import java.nio.file.Files
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/** The inbox and notification settings as layouts (D52, decision `layouts-not-apps`) over [FakeEngineBridge] and `app/desktopApp/src/sim/resources/notifications.json`. */
@OptIn(ExperimentalTestApi::class)
class NotificationsSimTest {
    private val outDir: File = Files.createTempDirectory("construct-notifications-test").toFile()

    @AfterTest
    fun tearDown() {
        outDir.deleteRecursively()
    }

    private fun fixture(name: String): String = assertNotNull(findFixture(name), "${fixturePath(name)} not found").readText()

    private val notifications = FakeNotifications.fromFixture(fixture("notifications.json"))
    private val screenData = fixture("pixelfed_screen_data.json")

    /** A run like the simulator's fixture mode: [start] is the root, the inbox and other layouts present over it. */
    private fun controller(start: String = "pixelfed_sample_no_login"): Pair<ConstructController, FakeEngineBridge> {
        val bundles = mapOf("pixelfed_sample_no_login" to "pixelfed_sample_bundle.json", "pixelfed" to "pixelfed_feed_bundle.json")
        val bridge = FakeEngineBridge(
            fixture(bundles.getValue(start)), appId = start, screenData = screenData, notifications = notifications,
            layoutBundles = { id -> layoutBundle(id, File("").absoluteFile) },
        )
        return ConstructController(bridge, start) to bridge
    }

    private fun runner(controller: ConstructController) = SimRunner(
        controller, outDir, ScreenSpec.parse("phone"),
        notify = { app, channel, title -> notifications.inject(app, channel, title) },
    )

    private fun SimRunner.ok(script: String): SimResult = run(parseScript(script)).also {
        assertTrue(it.ok, it.steps.joinToString("\n") { s -> s.logLine() })
    }

    private fun SkikoComposeUiTest.has(tag: String): Boolean =
        onAllNodes(hasTestTag(tag), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()

    @Test
    fun theInboxLayoutListsTheFixturesAndMarksThemRead() {
        val (controller, _) = controller()
        assertEquals(3, controller.unread.value.total, "three unread in the fixture")
        val live = mutableListOf<AppNotification>()
        controller.onNotification = { live += it }
        val r = runner(controller).ok(
            "notify pixelfed_sample_no_login likes Bob liked your post; open-inbox; assert-text Today; assert-text Yesterday; " +
                "assert-text @bob commented on your post; assert-text Bob liked your post; assert-text 4 unread; click Mark all read",
        )
        assertTrue(r.steps[1].message.startsWith("inbox shown (sheet)"), r.steps[1].message)
        assertEquals(listOf("Bob liked your post"), live.map { it.title }, "a live arrival reaches the platform hook")
        assertEquals(0, controller.unread.value.total)
    }

    @Test
    fun clickingANotificationOpensItsTargetScreenAndMarksItRead() {
        val (controller, _) = controller("pixelfed")
        runner(controller).ok("open-inbox; click @bob commented on your post; assert-text Comments; assert-text Which lens?")
        // The root is the notification's layout: the inbox closes and its screen opens over the root.
        val top = assertNotNull(controller.navigation.topScreen)
        assertEquals("comment_thread", top.screen.id)
        assertEquals(mapOf("id" to "781254316723458049", "comment" to "781300000000000107"), top.params)
        assertEquals(1, controller.navigation.stack.size, "the inbox closed")
        assertEquals(2, controller.unread.value.total)
    }

    @Test
    fun channelSwitchesSave() {
        val (controller, _) = controller("pixelfed")
        val r = runner(controller).ok("open-settings; toggle-channel pixelfed.likes; toggle-channel pixelfed.other")
        assertEquals("off", r.steps[1].message)
        assertEquals("on", r.steps[2].message)
        assertEquals(
            listOf(Triple("pixelfed", "likes", false), Triple("pixelfed", "other", true)),
            notifications.channelChanges,
        )
        assertEquals(false, notifications.channel("pixelfed", "likes")?.enabled)
    }

    @Test
    fun aFakeWithoutNotificationsListsNone() {
        val fake = FakeEngineBridge(fixture("pixelfed_feed_bundle.json"), layoutBundles = { layoutBundle(it, File("").absoluteFile) })
        val controller = ConstructController(fake, fake.appId)
        val r = runner(controller).ok("open-inbox; assert-text No notifications yet.")
        assertTrue(r.steps[0].message.endsWith("0 notification(s)"), r.steps[0].message)
        assertEquals(0, controller.unread.value.total)
        val failed = SimRunner(controller, outDir, ScreenSpec.parse("phone")).run(parseScript("notify a b c"))
        assertFalse(failed.ok, "notify needs the fixture mode's notifications")
    }
}
