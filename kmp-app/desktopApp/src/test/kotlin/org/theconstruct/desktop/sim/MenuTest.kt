package org.theconstruct.desktop.sim

import androidx.compose.ui.geometry.Size
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.SkikoComposeUiTest
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.runSkikoComposeUiTest
import androidx.compose.ui.unit.Density
import org.theconstruct.ConstructApp
import org.theconstruct.ConstructController
import org.theconstruct.ConstructEngineBridge
import org.theconstruct.ConstructJson
import org.theconstruct.EngineSnapshot
import org.theconstruct.LayoutApiException
import org.theconstruct.ServerChoice
import org.theconstruct.ServerOption
import org.theconstruct.UiNode
import org.theconstruct.displayedWorkspace
import java.io.File
import java.nio.file.Files
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** The hamburger menu and the version picker over [FakeEngineBridge], through the simulator's commands. */
@OptIn(ExperimentalTestApi::class)
class MenuTest {
    private val outDir: File = Files.createTempDirectory("construct-menu-test").toFile()

    @AfterTest
    fun tearDown() {
        outDir.deleteRecursively()
    }

    private fun bundle(name: String): String = assertNotNull(findFixture(name), "fixtures/$name not found").readText()

    private fun feed() = FakeEngineBridge(bundle("pixelfed_home_feed_bundle.json"))

    private fun runner(fake: FakeEngineBridge, controller: ConstructController = ConstructController(fake, fake.appId)) =
        SimRunner(
            controller, outDir, ScreenSpec.parse("phone"),
            setOnline = fake::setOnline, publishVersion = fake::publishVersion,
        )

    private fun SimRunner.ok(script: String): SimResult = run(parseScript(script)).also {
        assertTrue(it.ok, it.steps.joinToString("\n") { s -> s.logLine() })
    }

    private fun SkikoComposeUiTest.has(tag: String): Boolean =
        onAllNodes(hasTestTag(tag), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()

    private fun compose(controller: ConstructController, server: ServerChoice? = null, block: SkikoComposeUiTest.() -> Unit) {
        SimImages.install("fixture")
        runSkikoComposeUiTest(size = Size(390f, 844f), density = Density(1f)) {
            setContent { ConstructApp(controller, server = server) }
            waitForIdle()
            block()
        }
    }

    @Test
    fun defaultViewIsTheLayoutAndTheMenuButtonOnly() {
        val controller = ConstructController(feed())
        compose(controller) {
            assertTrue(has("menu-button"))
            assertFalse(has("menu-badge"))
            assertFalse(has("menu-drawer"), "the closed drawer adds nothing to the screen")
            assertTrue(onAllNodes(hasText("Online")).fetchSemanticsNodes().isEmpty(), "no status bar")
            assertTrue(onAllNodes(hasText("Edit layout with AI")).fetchSemanticsNodes().isEmpty())
            onNode(hasText("Tram lines", substring = true)).assertExists()
        }
    }

    @Test
    fun menuListsAppsConnectionAccountAndServer() {
        val r = runner(feed()).ok(
            "open-menu; assert-text Pixelfed demo; assert-text Counter; assert-text Latest · v3; assert-text Online; " +
                "assert-text Not signed in; assert-text Staging; close-menu; assert-no-text Connection",
        )
        assertEquals("3 app(s)", r.steps[0].message)
    }

    @Test
    fun selectingAnOlderVersionRendersItsSpecUntilLatest() {
        val fake = feed()
        val controller = ConstructController(fake, fake.appId)
        val r = runner(fake, controller).ok(
            "assert-text Comment; open-history Pixelfed; assert-text First feed: caption above the photo; " +
                "assert-text 9 days ago; select-version 2; assert-no-text Comment; assert-text Tram lines",
        )
        assertEquals("3 version(s) listed", r.steps[1].message)
        assertEquals("showing v2", r.steps[4].message)
        val pin = assertNotNull(controller.pinned.value)
        assertEquals(2L, pin.version)
        val base = ConstructJson.decodeBundle(bundle("pixelfed_home_feed_bundle.json")).workspace
        assertEquals(FakeAppCatalog.withoutActionBar(base), displayedWorkspace(controller.state.value, pin))
        // Client-local: the fake pod still serves v3.
        assertEquals(3L, fake.layoutVersion)
        runner(fake, controller).ok("open-history pixelfed_home; select-version latest; assert-text Comment")
        assertNull(controller.pinned.value)
    }

    @Test
    fun versionsThisBuildCantDrawAreGreyedOutAndCantBeSelected() {
        val base = ConstructJson.decodeBundle(bundle("pixelfed_home_feed_bundle.json")).workspace
        val catalog = FakeAppCatalog()
        catalog.add(
            "pixelfed_home", "Pixelfed", "photo",
            FakeAppCatalog.Version(FakeAppCatalog.captionFirst(base), "Old feed", 0, schema = 1, incompatible = "retired"),
            FakeAppCatalog.Version(base, "Current", 0, schema = 2),
            FakeAppCatalog.Version(base, "Stories", 0, schema = 3, incompatible = "update_required"),
        )
        val fake = FakeEngineBridge(bundle("pixelfed_home_feed_bundle.json"), apps = catalog)
        val controller = ConstructController(fake, fake.appId)
        compose(controller) {
            controller.menu.showHistory("pixelfed_home")
            waitForIdle()
            waitUntil(timeoutMillis = 5_000) { has("version-1") }
            onNode(hasTestTag("version-3")).assertIsNotEnabled()
            onNode(hasTestTag("version-1")).assertIsNotEnabled()
            onNode(hasTestTag("version-2")).assertIsEnabled()
            onNode(hasText("Needs a newer version of the app · Stories")).assertExists()
            onNode(hasText("Made for an older version of the app · Old feed")).assertExists()
        }
        val r = runner(fake, controller).run(parseScript("open-history Pixelfed; select-version 1"))
        assertFalse(r.ok)
        assertTrue(
            r.steps.last().message.contains("v1 can't be shown by this build: v1"),
            r.steps.joinToString("\n") { it.logLine() },
        )
        assertNull(controller.pinned.value)
        runner(fake, controller).ok("open-history Pixelfed; select-version 2")
        assertEquals(2L, controller.pinned.value?.version)
    }

    @Test
    fun publishingANewerVersionKeepsThePinBadgesAndOffersLatest() {
        val fake = feed()
        val controller = ConstructController(fake, fake.appId)
        runner(fake, controller).ok(
            "open-history Pixelfed; select-version 3; publish-version Stories row; assert-no-text What's new; " +
                "open-menu; assert-text Newer version available; assert-text You're viewing v3, v4 is the latest; " +
                "assert-text Showing v3 · latest v4",
        )
        compose(controller) {
            assertTrue(has("menu-badge"), "a newer version badges the menu")
            controller.menu.closeMenu()
            waitForIdle()
            assertTrue(has("menu-badge"))
        }
        runner(fake, controller).ok("open-menu; click tag:menu-newer-latest; assert-text What's new: Stories row")
        assertNull(controller.pinned.value)
        compose(controller) { assertFalse(has("menu-badge")) }
    }

    @Test
    fun followingLatestShowsNewVersionsAtOnce() {
        val fake = feed()
        runner(fake).ok("publish-version Stories row; assert-text What's new: Stories row; open-menu; assert-text Latest · v4")
    }

    @Test
    fun offlineAndPendingBadgeTheMenu() {
        val fake = FakeEngineBridge(bundle("counter_home_bundle.json"))
        val controller = ConstructController(fake)
        compose(controller) {
            assertFalse(has("menu-badge"))
            fake.setOnline(false)
            waitForIdle()
            assertTrue(has("menu-badge"), "offline")
        }
        runner(fake, controller).ok(
            "click Increment; open-menu; assert-text Offline; assert-text 1 pending; " +
                "assert-text Can't reach your pod; assert-text Counter",
        )
        fake.setOnline(true)
        compose(controller) { assertFalse(has("menu-badge")) }
    }

    @Test
    fun coldStartShowsNoOfflineBadgeUntilTheFirstSnapshot() {
        val silent = object : ConstructEngineBridge {
            override fun subscribe(onState: (EngineSnapshot) -> Unit) {}
            override fun dispatch(intentJson: String) {}
        }
        val controller = ConstructController(silent, "pixelfed_home")
        compose(controller) {
            assertFalse(has("menu-badge"), "connectivity is unknown, not offline")
            controller.menu.openMenu()
            waitForIdle()
            onNode(hasText("Connecting…")).assertExists()
            assertTrue(onAllNodes(hasText("Offline")).fetchSemanticsNodes().isEmpty())
        }
    }

    @Test
    fun offlineDrawerKeepsTheListWithoutAnErrorAndRefreshesWhenBackOnline() {
        val fake = feed()
        val controller = ConstructController(fake, fake.appId)
        runner(fake, controller).ok(
            "open-menu; assert-text Counter; close-menu; offline; open-menu; assert-text Offline; assert-text Counter; " +
                "assert-text Pixelfed demo; assert-no-text Can't reach your pod; assert-no-text Try again",
        )
        assertTrue(controller.menu.appsFailedOffline)
        runner(fake, controller).ok("online; open-menu; assert-text Online; assert-text Counter")
        assertFalse(controller.menu.appsFailedOffline, "the list was reloaded once back online")
    }

    @Test
    fun anOfflineKindFailureWhileOnlineShowsTryAgain() {
        val fake = feed()
        var fail = false
        // Online, but the apps call fails like a timeout or a closed engine (Kind.Offline).
        val bridge = object : ConstructEngineBridge by fake {
            override fun listApps(): String =
                if (fail) throw LayoutApiException("Can't reach your pod.", LayoutApiException.Kind.Offline) else fake.listApps()
        }
        val controller = ConstructController(bridge, fake.appId)
        runner(fake, controller).ok("open-menu; assert-text Counter; close-menu")
        fail = true
        runner(fake, controller).ok("open-menu; assert-text Online; assert-text Counter; assert-text Can't reach your pod.; assert-text Try again")
    }

    @Test
    fun aFailedServerSwitchIsShownInTheServerSection() {
        val controller = ConstructController(feed())
        val error = ServerChoice.switchFailed("https://example.com/", IllegalStateException("boom"))
        assertEquals("Couldn't switch to https://example.com: boom", error)
        val server = ServerChoice("https://staging.example", listOf(ServerOption("staging", "Staging", "https://staging.example")), error = error) {}
        compose(controller, server) {
            controller.menu.openMenu()
            waitForIdle()
            onNode(hasTestTag("server-error"), useUnmergedTree = true).assertExists()
            onNode(hasText(error)).assertExists()
        }
    }

    @Test
    fun aPinOnAnOlderPageStaysVisibleAsAPinnedRow() {
        val fake = feed()
        repeat(22) { fake.publishVersion("Change ${it + 1}") }
        val controller = ConstructController(fake, fake.appId)
        val r = runner(fake, controller).ok(
            "open-history Pixelfed; select-version 2; open-history Pixelfed; assert-text Pinned: v2; " +
                "click tag:versions-more; assert-no-text Pinned: v2; assert-text v2",
        )
        assertEquals("20 version(s) listed", r.steps[2].message)
        assertEquals(2L, controller.pinned.value?.version)
        compose(controller) {
            controller.menu.showHistory("pixelfed_home")
            waitForIdle()
            assertTrue(has("version-pinned"), "the pinned version is on page 2: shown as a checked row under Latest")
        }
    }

    @Test
    fun olderVersionsArePaged() {
        val fake = feed()
        repeat(22) { fake.publishVersion("Change ${it + 1}") }
        val controller = ConstructController(fake, fake.appId)
        val r = runner(fake, controller).ok("open-history Pixelfed; assert-text Load older; select-version 1; assert-text Sunrise over Boulder")
        assertEquals("20 version(s) listed", r.steps[0].message)
        assertEquals(1L, controller.pinned.value?.version)
    }

    @Test
    fun setServerRecordsTheChoice() {
        val fake = feed()
        val runner = runner(fake)
        runner.ok("set-server emulator; assert-text Local emulator; set-server http://192.168.1.5:8080/; assert-text http://192.168.1.5:8080")
        assertEquals(listOf("http://10.0.2.2:8080", "http://192.168.1.5:8080"), runner.serverSelections)
        assertEquals("http://192.168.1.5:8080", runner.serverUrl)
    }

    @Test
    fun openAppSwitchesEnginesWhenThePlatformCan() {
        val first = feed()
        var opened: FakeEngineBridge? = null
        val controller = ConstructController(first, first.appId) { id ->
            FakeEngineBridge(bundle("counter_home_bundle.json"), appId = id, apps = first.apps).also { opened = it }
        }
        runner(first, controller).ok("open-history Pixelfed; select-version 2; open-app Counter; assert-text Count: 0")
        assertEquals("home", controller.currentAppId())
        assertNull(controller.pinned.value, "a pin belongs to its app")
        assertNotNull(opened)
        // Snapshots of the closed engine are ignored.
        first.setOnline(false)
        assertEquals(true, controller.state.value.isOnline)
        assertTrue((controller.state.value.workspace as UiNode.Workspace).id == "home")
    }

    @Test
    fun openAppIsSafeWithoutAnEngineFactory() {
        val fake = feed()
        val controller = ConstructController(fake, fake.appId)
        val r = runner(fake, controller).run(parseScript("open-app Counter"))
        assertFalse(r.ok)
        assertTrue("isn't available here" in r.steps[0].message, r.steps[0].message)
        assertEquals("pixelfed_home", controller.currentAppId())
    }

    @Test
    fun backClosesThePickerThenTheMenu() {
        val fake = feed()
        val controller = ConstructController(fake, fake.appId)
        runner(fake, controller).ok("open-history Pixelfed; back; assert-text Connection; back; assert-no-text Connection")
        assertFalse(controller.menu.open)
    }
}
