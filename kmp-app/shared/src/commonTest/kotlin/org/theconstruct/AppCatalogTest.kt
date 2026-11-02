package org.theconstruct

import kotlinx.serialization.json.JsonPrimitive
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertSame
import kotlin.test.assertTrue

class AppCatalogTest {
    private val feed = ConstructJson.decodeBundle(PIXELFED_BUNDLE_JSON).workspace
    private val older = UiNode.Workspace("pixelfed_home", "Old", emptyList())

    private fun state(version: Long?, online: Boolean = true, pending: Int = 0, appId: String? = "pixelfed_home") =
        ConstructState(workspace = feed, isOnline = online, pendingActions = pending, seq = 1, activeLayout = ActiveLayout(appId, version))

    @Test
    fun activeLayoutReadsTheNewAndTheOldShape() {
        assertEquals(ActiveLayout("pixelfed_home", 3), ActiveLayout.parse("""{"app_id":"pixelfed_home","version":3}"""))
        assertEquals(ActiveLayout("pixelfed_home", null), ActiveLayout.parse("""{"app_id":"pixelfed_home"}"""))
        assertEquals(ActiveLayout("home", 5), ActiveLayout.parse("""{"layout_id":"home.counter","layout_version":2,"revision":5}"""))
        assertEquals(ActiveLayout(), ActiveLayout.parse("{}"))
        assertEquals(ActiveLayout(), ActiveLayout.parse("not json"))
        assertEquals(ActiveLayout(null, null), ActiveLayout.parse("""{"app_id":7,"version":"2"}"""))
    }

    @Test
    fun parsesAppsAndVersions() {
        val apps = AppCatalogJson.parseApps(
            """{"apps":[{"id":"pixelfed_home","title":"Pixelfed","icon":"photo","latest_version":3,"updated_at":"2026-11-02T12:00:00Z"},
               {"id":"home"},{"title":"no id"},7]}""",
        )
        assertEquals(
            listOf(AppSummary("pixelfed_home", "Pixelfed", "photo", 3, 1_793_620_800), AppSummary("home", "home")),
            apps,
        )
        val page = AppCatalogJson.parseVersions(
            """{"app_id":"pixelfed_home","latest_version":9,"versions":[
                 {"version":9,"created_at":"2026-11-02T12:00:00.5+01:00","source":"file","note":"Stories"},
                 {"version":8,"created_at":1700000000,"source":"agent","note":""},{"source":"seed"}],"next_before":8}""",
        )
        assertEquals(
            AppVersionPage(
                listOf(AppVersionInfo(9, 1_793_617_200, "file", "Stories"), AppVersionInfo(8, 1_700_000_000, "agent", null)),
                nextBefore = 8, latestVersion = 9,
            ),
            page,
        )
        assertEquals("Stories", versionDetail(page.versions[0]))
        assertEquals("AI edit", versionDetail(page.versions[1]))
        assertEquals("Published", versionSourceLabel("file"))
        assertEquals("Layout switch", versionSourceLabel("switch"))
        assertEquals("Weird", versionSourceLabel("weird"))
        val layout = AppCatalogJson.parseVersionLayout("""{"version":1,"layout":${ConstructJson.encodeNode(older)}}""")
        assertEquals(older, layout)
        assertFailsWith<IllegalArgumentException> { AppCatalogJson.parseVersionLayout("""{"version":1}""") }
    }

    @Test
    fun timestamps() {
        assertEquals(0L, parseTimestamp(JsonPrimitive("yesterday")))
        assertEquals(0L, parseTimestamp(null))
        assertEquals(0L, parseTimestamp(JsonPrimitive("1970-01-01T00:00:00Z")))
        assertEquals(951_782_400L, parseTimestamp(JsonPrimitive("2000-02-29T00:00:00Z")))
        assertEquals(42L, parseTimestamp(JsonPrimitive(42)))
        assertEquals("just now", relativeTime(100, 130))
        assertEquals("3 h ago", relativeTime(0, 3 * 3_600 + 5))
        assertEquals("yesterday", relativeTime(0, 30 * 3_600))
        assertEquals("9 days ago", relativeTime(0, 9 * 86_400))
        assertNull(relativeTime(0, 40 * 86_400))
        assertEquals("", versionTimeLabel(0, 100))
        assertEquals("5 min ago", versionTimeLabel(1_000, 1_300))
    }

    @Test
    fun historyPaging() {
        val first = VersionHistory().withFirstPage(AppVersionPage(listOf(AppVersionInfo(30), AppVersionInfo(29)), nextBefore = 29))
        assertTrue(first.hasMore)
        val loading = first.copy(loadingMore = true)
        val more = loading.withOlderPage(29, AppVersionPage(listOf(AppVersionInfo(29), AppVersionInfo(28), AppVersionInfo(27)), null))
        assertEquals(listOf(30L, 29L, 28L, 27L), more.versions.map { it.version })
        assertFalse(more.hasMore)
        assertFalse(more.loadingMore)
        // A reload replaced the history meanwhile: the late page is dropped.
        val reloaded = VersionHistory().withFirstPage(AppVersionPage(listOf(AppVersionInfo(31)), nextBefore = 31)).copy(loadingMore = true)
        val dropped = reloaded.withOlderPage(29, AppVersionPage(listOf(AppVersionInfo(28)), null))
        assertEquals(listOf(31L), dropped.versions.map { it.version })
        assertFalse(dropped.loadingMore)
        assertFalse(VersionHistory().hasMore)
    }

    @Test
    fun pinShowsThePinnedSpecOnlyOnAVersionedScreenOfItsApp() {
        val pin = PinnedVersion("pixelfed_home", 2, older)
        assertSame(feed, displayedWorkspace(state(3), null))
        assertSame(older, displayedWorkspace(state(3), pin))
        // Signed-out screen (no version): shown as is.
        assertSame(feed, displayedWorkspace(state(null), pin))
        // Another app on screen.
        assertSame(feed, displayedWorkspace(state(3, appId = "home"), pin))
        // Nothing loaded yet.
        assertNull(displayedWorkspace(ConstructState(), pin))
    }

    @Test
    fun newerVersionAndBadgeRules() {
        val pinnedLatest = PinnedVersion("pixelfed_home", 3, older)
        assertFalse(newerVersionAvailable(state(3), pinnedLatest))
        assertFalse(showMenuBadge(state(3), pinnedLatest))
        // The pod published v4 while v3 is pinned: keep v3, say so, badge.
        assertTrue(newerVersionAvailable(state(4), pinnedLatest))
        assertTrue(showMenuBadge(state(4), pinnedLatest))
        assertSame(older, displayedWorkspace(state(4), pinnedLatest))
        // Following the latest: never "newer".
        assertFalse(newerVersionAvailable(state(4), null))
        assertFalse(showMenuBadge(state(4), null))
        // Offline or pending mutations badge too; the signed-out screen does not count as pinned.
        assertTrue(showMenuBadge(state(3, online = false), null))
        assertTrue(showMenuBadge(state(3, pending = 2), null))
        assertFalse(showMenuBadge(state(null), pinnedLatest.copy(version = 1)))
    }

    @Test
    fun controllerPinAndLatest() {
        val bridge = object : ConstructEngineBridge {
            override fun subscribe(onState: (EngineSnapshot) -> Unit) {}
            override fun dispatch(intentJson: String) {}
        }
        val controller = ConstructController(bridge, "pixelfed_home")
        controller.pin("pixelfed_home", 2, older)
        assertEquals(PinnedVersion("pixelfed_home", 2, older), controller.pinned.value)
        controller.followLatest()
        assertNull(controller.pinned.value)
        // A bridge without apps throws a readable error; the old catalog calls still exist for the FFI bridge.
        val e = assertFailsWith<LayoutApiException> { bridge.listApps() }
        assertEquals("Apps aren't available here.", e.message)
        assertFailsWith<LayoutApiException> { bridge.appVersions("pixelfed_home", before = 3, limit = 20) }
        assertFailsWith<LayoutApiException> { bridge.appVersion("pixelfed_home", 1) }
    }

    @Test
    fun serverUrlValidation() {
        assertEquals("http://10.0.2.2:8080", ServerUrls.parse(" http://10.0.2.2:8080/ "))
        assertEquals("https://the-construct-pod.fly.dev", ServerUrls.parse("https://the-construct-pod.fly.dev"))
        assertEquals("http://localhost", ServerUrls.parse("http://localhost"))
        assertEquals("http://[::1]:8080", ServerUrls.parse("http://[::1]:8080"))
        for (bad in listOf(
            "", "http://", "ftp://host", "host:8080", "http://host:0", "http://host:65536", "http://host/path",
            "http://host?q=1", "http://ho st", "http://-host", "http://host:80a", "http://host#x",
        )) {
            assertNull(ServerUrls.parse(bad), bad)
        }
        val choice = ServerChoice("http://10.0.2.2:8080/", listOf(ServerOption("emulator", "Local emulator", "http://10.0.2.2:8080"))) {}
        assertEquals("Local emulator", choice.label())
        assertEquals("http://x:1", choice.label("http://x:1"))
    }

    @Test
    fun signInComesFromTheScreensOwnButton() {
        assertNull(findSignInIntent(feed))
        val signedOut = UiNode.Workspace(
            "pixelfed_home", "Home",
            listOf(
                SlotSpec(
                    "s", "core://x",
                    UiNode.Container(
                        Orientation.Vertical, 0,
                        listOf(UiNode.Text("Sign in", TextStyle.Title), UiNode.Button("Sign in with Pixelfed", ActionIntent.AuthLogin("https://pixelfed.test"))),
                    ),
                ),
            ),
        )
        assertEquals(ActionIntent.AuthLogin("https://pixelfed.test"), findSignInIntent(signedOut))
        assertNull(findSignInIntent(null))
    }
}
