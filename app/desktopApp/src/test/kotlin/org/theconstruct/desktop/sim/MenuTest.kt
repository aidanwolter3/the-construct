package org.theconstruct.desktop.sim

import androidx.compose.ui.geometry.Size
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.SkikoComposeUiTest
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.performClick
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

    private fun bundle(name: String): String = assertNotNull(findFixture(name), "${fixturePath(name)} not found").readText()

    private fun feed() = FakeEngineBridge(bundle("pixelfed_feed_bundle.json"))

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
    fun menuListsAppsAndSettingsHoldsThePodAndSignIns() {
        val r = runner(feed()).ok(
            "open-menu; assert-text Pixelfed sample; assert-text Latest · v3; assert-text Settings; assert-text Online; " +
                "assert-no-text Not signed in; assert-no-text Staging; " +
                "open-settings; assert-text Your pod; assert-text Online; assert-text Staging; assert-text Not signed in; " +
                "close-menu; assert-no-text Your pod; assert-no-text Version history of Pixelfed",
        )
        assertEquals("0 sign-in(s)", r.steps[7].message)
        // Pixelfed, Pixelfed sample and the fixture's custom Kitchen.
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
        val base = ConstructJson.decodeBundle(bundle("pixelfed_feed_bundle.json")).workspace
        assertEquals(FakeAppCatalog.withoutActionBar(base), displayedWorkspace(controller.state.value, pin))
        // Client-local: the fake pod still serves v3.
        assertEquals(3L, fake.layoutVersion)
        runner(fake, controller).ok("open-history pixelfed; select-version latest; assert-text Comment")
        assertNull(controller.pinned.value)
    }

    @Test
    fun versionsThisBuildCantDrawAreGreyedOutAndCantBeSelected() {
        val base = ConstructJson.decodeBundle(bundle("pixelfed_feed_bundle.json")).workspace
        val catalog = FakeAppCatalog()
        catalog.add(
            "pixelfed", "Pixelfed", "photo",
            FakeAppCatalog.Version(FakeAppCatalog.captionFirst(base), "Old feed", 0, incompatible = "retired"),
            FakeAppCatalog.Version(base, "Current", 0),
            FakeAppCatalog.Version(base, "Stories", 0, incompatible = "update_required"),
        )
        val fake = FakeEngineBridge(bundle("pixelfed_feed_bundle.json"), apps = catalog)
        // Like a pod, the fake serves the newest version this build can draw.
        assertEquals(2L, fake.layoutVersion)
        val controller = ConstructController(fake, fake.appId)
        compose(controller) {
            controller.menu.showHistory("pixelfed")
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
        // v3 needs a newer build: hinted, but never "Newer version available" and no badge.
        runner(fake, controller).ok(
            "open-menu; assert-no-text Newer version available; assert-text Showing v2 · latest v2 · Update the app to see v3",
        )
        compose(controller) {
            assertFalse(has("menu-badge"), "a version this build can't draw does not badge the menu")
            controller.menu.showHistory("pixelfed")
            waitForIdle()
            waitUntil(timeoutMillis = 5_000) { has("version-1") }
            onNode(hasText("Follows new versions as they're published. Update the app to see v3")).assertExists()
        }
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
            "click Increment; open-menu; assert-text Offline · 1 pending; assert-text Version history of Home; " +
                "assert-text Can't reach your pod; open-settings; assert-text Offline; assert-text 1 pending; " +
                "assert-text Changes are saved and sent when you're back online",
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
        val controller = ConstructController(silent, "pixelfed")
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
            "open-menu; assert-text Pixelfed sample; close-menu; offline; open-menu; assert-text Offline; " +
                "assert-text Pixelfed sample; assert-no-text Can't reach your pod; assert-no-text Try again",
        )
        assertTrue(controller.menu.appsFailedOffline)
        runner(fake, controller).ok("online; open-menu; assert-text Online; assert-text Pixelfed sample")
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
        runner(fake, controller).ok("open-menu; assert-text Pixelfed sample; close-menu")
        fail = true
        runner(fake, controller).ok(
            "open-menu; assert-text Online; assert-text Pixelfed sample; assert-text Can't reach your pod.; assert-text Try again",
        )
    }

    @Test
    fun aFailedServerSwitchIsShownInTheServerSection() {
        val controller = ConstructController(feed())
        val error = ServerChoice.switchFailed("https://example.com/", IllegalStateException("boom"))
        assertEquals("Couldn't switch to https://example.com: boom", error)
        val server = ServerChoice("https://staging.example", listOf(ServerOption("staging", "Staging", "https://staging.example")), error = error) {}
        compose(controller, server) {
            controller.menu.showSettings()
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
            controller.menu.showHistory("pixelfed")
            waitForIdle()
            assertTrue(has("version-pinned"), "the pinned version is on page 2: shown as a checked row under Latest")
        }
    }

    @Test
    fun olderVersionsArePaged() {
        val fake = feed()
        repeat(22) { fake.publishVersion("Change ${it + 1}") }
        val controller = ConstructController(fake, fake.appId)
        val r = runner(fake, controller).ok("open-history Pixelfed; assert-text Load older; select-version 1; assert-text Sunrise over Millbrook")
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
            FakeEngineBridge(bundle("pixelfed_sample_bundle.json"), appId = id, apps = first.apps).also { opened = it }
        }
        runner(first, controller).ok("open-history Pixelfed; select-version 2; open-app pixelfed_sample_no_login; assert-text Sunrise over Millbrook")
        assertEquals("pixelfed_sample_no_login", controller.currentAppId())
        assertNull(controller.pinned.value, "a pin belongs to its app")
        assertNotNull(opened)
        // Snapshots of the closed engine are ignored.
        first.setOnline(false)
        assertEquals(true, controller.state.value.isOnline)
        // The sample bundle's root id (the app id is pixelfed_sample_no_login).
        assertEquals("home_stream", (controller.state.value.workspace as UiNode.Workspace).id)
    }

    @Test
    fun openAppIsSafeWithoutAnEngineFactory() {
        val fake = feed()
        val controller = ConstructController(fake, fake.appId)
        val r = runner(fake, controller).run(parseScript("open-app pixelfed_sample_no_login"))
        assertFalse(r.ok)
        assertTrue("isn't available here" in r.steps[0].message, r.steps[0].message)
        assertEquals("pixelfed", controller.currentAppId())
    }

    @Test
    fun backClosesThePickerThenTheMenu() {
        val fake = feed()
        val controller = ConstructController(fake, fake.appId)
        runner(fake, controller).ok("open-history Pixelfed; back; assert-text Version history of Pixelfed; back; assert-no-text Version history of Pixelfed")
        assertFalse(controller.menu.open)
    }

    @Test
    fun aSignInOpensItsDetailAndBackWalksOutLevelByLevel() {
        val fake = FakeEngineBridge(
            bundle("pixelfed_feed_bundle.json"),
            authJson = """{"state":"signed_in","username":"constructdemo"}""",
        )
        val controller = ConstructController(fake, fake.appId)
        val r = runner(fake, controller).ok(
            "open-settings; assert-text Pixelfed; assert-text @constructdemo · 127.0.0.1:8081; " +
                "click tag:sign-in-pixelfed; assert-text Server; assert-text 127.0.0.1:8081; assert-text Username; " +
                "assert-text Used by; back; assert-text Sign-ins; back; assert-text Version history of Pixelfed; " +
                "assert-no-text Sign-ins; back; assert-no-text Version history of Pixelfed",
        )
        assertEquals("1 sign-in(s)", r.steps[0].message)
        assertFalse(controller.menu.open)
        // close-menu walks out of a detail page too.
        runner(fake, controller).ok("open-settings; click tag:sign-in-pixelfed; close-menu; assert-no-text Used by")
        assertFalse(controller.menu.open)
    }

    @Test
    fun settingsFallsBackToTheEngineAccountWhenThePodListsNone() {
        // A pod without `/v1/auth/sign-ins` lists nothing (the bridge default): the engine's own account still shows, with Sign out.
        val fake = FakeEngineBridge(
            bundle("pixelfed_feed_bundle.json"),
            authJson = """{"state":"signed_in","username":"constructdemo"}""",
        )
        val bridge = object : ConstructEngineBridge by fake {
            override fun signIns(): String = """{"sign_ins":[]}"""
        }
        val controller = ConstructController(bridge, fake.appId)
        runner(fake, controller).ok(
            "open-settings; assert-text Signed in as @constructdemo; click tag:menu-sign-out; assert-text Not signed in",
        )
    }

    /** The feed engine, opening other apps of the shared fixture catalog on their own fixtures. */
    private fun switching(): Pair<FakeEngineBridge, ConstructController> {
        val first = feed()
        val controller = ConstructController(first, first.appId, defaultAppId = "pixelfed") { id ->
            val name = when (id) {
                FakeAppCatalog.KITCHEN -> "custom_kitchen_bundle.json"
                "pixelfed_sample_no_login" -> "pixelfed_sample_bundle.json"
                else -> "pixelfed_feed_bundle.json"
            }
            FakeEngineBridge(bundle(name), appId = id, apps = first.apps)
        }
        return first to controller
    }

    @Test
    fun customAppsAreMarkedAndAnyAppCanBeHiddenAndShownAgain() {
        val (_, controller) = switching()
        compose(controller) {
            controller.menu.openMenu()
            waitForIdle()
            waitUntil(timeoutMillis = 5_000) { has("app-${FakeAppCatalog.KITCHEN}") }
            assertTrue(has("app-custom-${FakeAppCatalog.KITCHEN}"), "the custom marker")
            assertFalse(has("app-custom-pixelfed"))
            onNode(hasTestTag("app-more-pixelfed_sample_no_login")).performClick()
            waitForIdle()
            onNode(hasTestTag("app-hide-pixelfed_sample_no_login")).performClick()
            waitUntil(timeoutMillis = 5_000) { !has("app-pixelfed_sample_no_login") }
            onNode(hasText("Show hidden apps (1)")).performClick()
            waitForIdle()
            assertTrue(has("app-pixelfed_sample_no_login"))
            onNode(hasText("Hidden", substring = true)).assertExists()
            onNode(hasTestTag("app-more-pixelfed_sample_no_login")).performClick()
            waitForIdle()
            onNode(hasText("Show in the app list")).performClick()
            waitUntil(timeoutMillis = 5_000) { !has("apps-show-hidden") }
            // Built-ins have no Delete.
            onNode(hasTestTag("app-more-pixelfed")).performClick()
            waitForIdle()
            assertFalse(has("app-delete-pixelfed"))
        }
    }

    @Test
    fun deletingTheCustomAppOnScreenAsksThenFallsBackToTheDefaultApp() {
        val (first, controller) = switching()
        compose(controller) {
            assertTrue(controller.openApp(FakeAppCatalog.KITCHEN))
            waitUntil(timeoutMillis = 5_000) { onAllNodes(hasText("Kitchen")).fetchSemanticsNodes().isNotEmpty() }
            controller.menu.openMenu()
            waitForIdle()
            waitUntil(timeoutMillis = 5_000) { has("app-more-${FakeAppCatalog.KITCHEN}") }
            onNode(hasTestTag("app-more-${FakeAppCatalog.KITCHEN}")).performClick()
            waitForIdle()
            onNode(hasTestTag("app-delete-${FakeAppCatalog.KITCHEN}")).performClick()
            waitForIdle()
            onNode(hasText("Delete Kitchen?")).assertExists()
            onNode(hasTestTag("delete-app-cancel")).performClick()
            waitForIdle()
            assertTrue(first.apps.has(FakeAppCatalog.KITCHEN), "Cancel keeps it")
            onNode(hasTestTag("app-more-${FakeAppCatalog.KITCHEN}")).performClick()
            waitForIdle()
            onNode(hasTestTag("app-delete-${FakeAppCatalog.KITCHEN}")).performClick()
            waitForIdle()
            onNode(hasTestTag("delete-app-confirm")).performClick()
            waitUntil(timeoutMillis = 5_000) { controller.currentAppId() == "pixelfed" }
            assertFalse(first.apps.has(FakeAppCatalog.KITCHEN))
            assertEquals(org.theconstruct.APP_DELETED, controller.notice.value)
            controller.menu.closeMenu()
            waitUntil(timeoutMillis = 5_000) { has("notice") }
            onNode(hasTestTag("notice-dismiss")).performClick()
            waitForIdle()
            assertNull(controller.notice.value)
        }
    }

    @Test
    fun aCopiedAppSaysItsBuiltInHasUpdatesUntilDismissed() {
        val (first, controller) = switching()
        compose(controller) {
            controller.menu.openMenu()
            waitForIdle()
            waitUntil(timeoutMillis = 5_000) { has("app-updates-${FakeAppCatalog.KITCHEN}") }
            onNode(hasText("Pixelfed sample has updates · ask Claude to bring them in")).assertExists()
            onNode(hasTestTag("app-updates-dismiss-${FakeAppCatalog.KITCHEN}")).performClick()
            waitUntil(timeoutMillis = 5_000) { !has("app-updates-${FakeAppCatalog.KITCHEN}") }
            assertTrue(first.apps.has(FakeAppCatalog.KITCHEN))
        }
    }

    @Test
    fun settingsOffersTheSignInsThePodHas() {
        val fake = feed()
        val controller = ConstructController(fake, fake.appId)
        compose(controller) {
            controller.menu.showSettings()
            waitForIdle()
            waitUntil(timeoutMillis = 5_000) { has("provider-pixelfed") }
            onNode(hasText("Sign in to Pixelfed")).assertExists()
            onNode(hasText("Not signed in")).assertExists()
            assertFalse(has("menu-sign-in"), "the provider row replaces the screen's button")
            onNode(hasTestTag("provider-pixelfed")).performClick()
            waitForIdle()
            assertFalse(controller.menu.open, "signing in closes the menu")
        }
    }
}
