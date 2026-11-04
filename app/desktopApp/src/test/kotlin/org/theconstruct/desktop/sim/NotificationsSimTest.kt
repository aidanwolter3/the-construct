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

/** The bell, the inbox and the notification settings (D52) over [FakeEngineBridge] and `app/desktopApp/src/sim/resources/notifications.json`. */
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

    /** A run like the simulator's fixture mode: [start] on screen, other apps openable, shared notifications. */
    private fun controller(start: String = "pixelfed_sample_no_login"): Pair<ConstructController, () -> FakeEngineBridge> {
        val bundles = mapOf("pixelfed_sample_no_login" to "pixelfed_sample_bundle.json", "pixelfed" to "pixelfed_feed_bundle.json")
        var current = FakeEngineBridge(fixture(bundles.getValue(start)), appId = start, screenData = screenData, notifications = notifications)
        val controller = ConstructController(current, start) { id ->
            FakeEngineBridge(
                fixture(bundles.getValue(id)), appId = id, apps = current.apps, screenData = screenData, notifications = notifications,
            ).also { current = it }
        }
        return controller to { current }
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
    fun theBellShowsTheUnreadCountAndTheInboxListsTheFixtures() {
        val (controller, _) = controller()
        assertEquals(3, controller.unread.value.total, "three unread in the fixture")
        val live = mutableListOf<AppNotification>()
        controller.onNotification = { live += it }
        SimImages.install("fixture")
        runSkikoComposeUiTest(size = Size(390f, 844f), density = Density(1f)) {
            setContent { ConstructApp(controller) }
            waitForIdle()
            assertTrue(has("inbox-button"))
            onNode(hasText("3") and hasParent(hasTestTag("inbox-badge")), useUnmergedTree = true).assertExists()

            notifications.inject("pixelfed_sample_no_login", "likes", "Bob liked your post")
            waitForIdle()
            assertEquals(listOf("Bob liked your post"), live.map { it.title }, "a live arrival reaches the platform hook")
            onNode(hasText("4") and hasParent(hasTestTag("inbox-badge")), useUnmergedTree = true).assertExists()

            onNode(hasTestTag("inbox-button")).performClick()
            waitUntil(timeoutMillis = 5_000) { has("notification-row:fx-1") || has("notification-row:fx-2") }
            assertTrue(has("inbox"))
            assertTrue(has("notification-row:sim-1"), "the live one is listed")
            onNode(hasText("Today")).assertExists()
            onNode(hasText("Yesterday")).assertExists()
            onNode(hasText("@bob commented on your post")).assertExists()

            onNode(hasTestTag("inbox-mark-all")).performClick()
            waitUntil(timeoutMillis = 5_000) { controller.unread.value.total == 0 }
            waitForIdle()
            assertFalse(has("inbox-badge"))
        }
    }

    @Test
    fun clickingANotificationOpensItsAppAndTargetScreenAndMarksItRead() {
        val (controller, current) = controller()
        runner(controller).ok(
            "notify pixelfed_sample_no_login likes Bob liked your post; open-inbox; click-notification commented on; " +
                "assert-text Comments; assert-text Which lens?",
        )
        assertEquals("pixelfed", controller.currentAppId())
        // A comment notification opens the whole thread at the comment (layout 0007).
        assertEquals("comment_thread", controller.navigation.top?.screen?.id)
        assertEquals(
            mapOf("id" to "781254316723458049", "comment" to "781300000000000107"),
            controller.navigation.top?.params,
        )
        assertEquals("pixelfed", current().appId)
        // fx-6 read; fx-5, fx-4 and the live one still unread.
        assertEquals(3, controller.unread.value.total)
    }

    @Test
    fun aNotificationWithoutATargetJustOpensItsApp() {
        val (controller, _) = controller("pixelfed")
        val r = runner(controller).ok("open-inbox; click-notification followed you")
        assertTrue(r.steps.last().message.contains("no screen"), r.steps.last().message)
        assertEquals(null, controller.navigation.top)
    }

    @Test
    fun channelSwitchesSaveAndShowTheirState() {
        val (controller, _) = controller("pixelfed")
        val r = runner(controller).ok("open-notification-settings; toggle-channel pixelfed.likes; toggle-channel pixelfed.other; close-menu")
        assertEquals("6 channel(s)", r.steps[0].message)
        assertEquals("off", r.steps[1].message)
        assertEquals("on", r.steps[2].message)
        assertEquals(
            listOf(Triple("pixelfed", "likes", false), Triple("pixelfed", "other", true)),
            notifications.channelChanges,
        )
        assertEquals(false, notifications.channel("pixelfed", "likes")?.enabled)
    }

    @Test
    fun aFakeWithoutNotificationsSaysSo() {
        val fake = FakeEngineBridge(fixture("pixelfed_feed_bundle.json"))
        val controller = ConstructController(fake, fake.appId)
        val r = runner(controller).ok("open-inbox; close-inbox")
        assertEquals("Notifications aren't available here.", r.steps[0].message)
        assertEquals(0, controller.unread.value.total)
        val failed = SimRunner(controller, outDir, ScreenSpec.parse("phone")).run(parseScript("notify a b c"))
        assertFalse(failed.ok, "notify needs the fixture mode's notifications")
    }
}
