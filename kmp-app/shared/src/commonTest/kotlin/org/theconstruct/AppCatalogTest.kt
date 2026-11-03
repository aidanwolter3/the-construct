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
    fun activeLayoutReadsAppIdAndVersion() {
        assertEquals(ActiveLayout("pixelfed_home", 3), ActiveLayout.parse("""{"app_id":"pixelfed_home","version":3}"""))
        assertEquals(ActiveLayout("pixelfed_home", null), ActiveLayout.parse("""{"app_id":"pixelfed_home"}"""))
        // The D27 shape is no longer read.
        assertEquals(ActiveLayout(), ActiveLayout.parse("""{"layout_id":"home.counter","layout_version":2,"revision":5}"""))
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
    fun versionsTheBuildCantDrawAreMarked() {
        val page = AppCatalogJson.parseVersions(
            """{"versions":[
                 {"version":3,"schema":3,"compatible":false,"incompatible":"update_required","note":"Stories"},
                 {"version":2,"schema":2,"compatible":true},
                 {"version":1,"schema":1,"compatible":false,"incompatible":"retired"}]}""",
        )
        assertEquals(
            listOf(
                AppVersionInfo(3, note = "Stories", schema = 3, incompatible = "update_required"),
                AppVersionInfo(2, schema = 2),
                AppVersionInfo(1, schema = 1, incompatible = "retired"),
            ),
            page.versions,
        )
        assertEquals(listOf(false, true, false), page.versions.map { it.compatible })
        assertEquals("Needs a newer version of the app · Stories", versionDetail(page.versions[0]))
        assertEquals("Made for an older version of the app", versionDetail(page.versions[2]))
        // An older pod says nothing: compatible.
        assertTrue(AppCatalogJson.parseVersions("""{"versions":[{"version":1}]}""").versions.single().compatible)
        // A spec the build can't draw is refused with the reason.
        val e = assertFailsWith<LayoutApiException> {
            AppCatalogJson.parseVersionLayout(
                """{"version":1,"compatible":false,"incompatible":"retired","layout":${ConstructJson.encodeNode(older)}}""",
            )
        }
        assertEquals("Made for an older version of the app.", e.message)
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
    fun theNewestVersionThisBuildCanDrawIsTheLatestItOffers() {
        val apps = AppCatalogJson.parseApps(
            """{"apps":[{"id":"a","latest_version":3,"latest_compatible_version":2},
               {"id":"b","latest_version":3,"latest_compatible_version":null},{"id":"c","latest_version":3}]}""",
        )
        assertEquals(listOf(2L, null, 3L), apps.map { it.latestCompatibleVersion })
        assertEquals(listOf(3L, 3L, null), apps.map { it.updateRequiredFor })
        val page = AppCatalogJson.parseVersions("""{"latest_version":3,"latest_compatible_version":2,"versions":[]}""")
        assertEquals(2L, page.latestCompatibleVersion)

        val a = apps[0]
        assertEquals("v2 · Update the app to see v3", appRowSubtitle(a, isCurrent = false, shown = 2, pinned = null))
        assertEquals("Latest · v2 · Update the app to see v3", appRowSubtitle(a, isCurrent = true, shown = 2, pinned = null))
        assertEquals(
            "Showing v1 · latest v2 · Update the app to see v3",
            appRowSubtitle(a, isCurrent = true, shown = 2, pinned = 1),
        )
        assertEquals("Update the app to see v3", appRowSubtitle(apps[1], isCurrent = false, shown = null, pinned = null))
        assertEquals("v3", appRowSubtitle(apps[2], isCurrent = false, shown = 3, pinned = null))
        // The engine already shows a version newer than the list says: no hint for it.
        assertEquals("Latest · v3", appRowSubtitle(a, isCurrent = true, shown = 3, pinned = null))

        val history = VersionHistory(
            listOf(AppVersionInfo(3, incompatible = "update_required"), AppVersionInfo(2), AppVersionInfo(1, incompatible = "retired")),
            loaded = true,
        )
        assertEquals("Follows new versions as they're published. Update the app to see v3", latestRowDetail(history, 2))
        assertEquals("Follows new versions as they're published", latestRowDetail(VersionHistory(listOf(AppVersionInfo(2)), loaded = true), 2))
    }

    @Test
    fun aVersionOnlyANewerBuildCanDrawIsNeverNewer() {
        // The engine shows what the pod serves this build (v2, the newest it can draw) though v3 exists: a pin on v2
        // is not behind, and nothing badges.
        val pin = PinnedVersion("pixelfed_home", 2, older)
        assertFalse(newerVersionAvailable(state(2), pin))
        assertFalse(showMenuBadge(state(2), pin))
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
        // Connectivity unknown (before the first snapshot): no badge.
        assertFalse(showMenuBadge(ConstructState(), null))
        assertFalse(showMenuBadge(state(3).copy(isOnline = null), null))
    }

    @Test
    fun pickerChecksThePinOnlyWhereItAppliesAndKeepsAnOlderPinVisible() {
        val pin = PinnedVersion("pixelfed_home", 2, older)
        assertEquals(pin, pinnedInPicker(state(25), pin, "pixelfed_home", isCurrent = true))
        // Signed out (no version): the pin doesn't apply, "Latest" is checked as in the Apps section.
        assertNull(pinnedInPicker(state(null), pin, "pixelfed_home", isCurrent = true))
        assertNull(pinnedInPicker(state(25), pin, "pixelfed_home", isCurrent = false))
        assertNull(pinnedInPicker(state(25), pin, "home", isCurrent = true))

        // 25 versions, pinned to v2: the first page (v25..v6) doesn't list it, so a "Pinned: v2" row shows.
        fun page(from: Long, to: Long, nextBefore: Long?) = AppVersionPage((from downTo to).map { AppVersionInfo(it) }, nextBefore)
        val first = VersionHistory().withFirstPage(page(25, 6, nextBefore = 6))
        assertEquals(20, first.versions.size)
        assertEquals(2L, pinnedRowVersion(pin, first))
        // Once "Load older" lists v2, its own row is checked instead.
        val all = first.withOlderPage(6, page(5, 1, nextBefore = null))
        assertNull(pinnedRowVersion(pin, all))
        // Nothing pinned, or the history not loaded yet: no row.
        assertNull(pinnedRowVersion(null, first))
        assertNull(pinnedRowVersion(pin, VersionHistory()))
        assertNull(pinnedRowVersion(pin.copy(version = 20), first))
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
        // Android's policy: https to any host, http only to the loopback / emulator hosts (images load over Java).
        val android = ServerUrls.ANDROID_CLEARTEXT_HOSTS
        assertEquals(ServerUrlCheck.Ok("https://192.168.1.5:8443"), ServerUrls.check("https://192.168.1.5:8443", android))
        assertEquals(ServerUrlCheck.Ok("https://pod.example.com"), ServerUrls.check("https://pod.example.com/", android))
        assertEquals(ServerUrlCheck.Ok("http://10.0.2.2:8080"), ServerUrls.check("http://10.0.2.2:8080", android))
        assertEquals(ServerUrlCheck.Ok("http://127.0.0.1:8080"), ServerUrls.check("http://127.0.0.1:8080", android))
        assertEquals(ServerUrlCheck.Ok("http://LocalHost:8080"), ServerUrls.check("http://LocalHost:8080", android))
        assertEquals(ServerUrlCheck.NeedsHttps, ServerUrls.check("http://192.168.1.5:8080", android))
        assertEquals(ServerUrlCheck.NeedsHttps, ServerUrls.check("http://pod.example.com", android))
        assertEquals(ServerUrlCheck.NeedsHttps, ServerUrls.check("http://10.0.2.20", android))
        assertEquals(ServerUrlCheck.Invalid, ServerUrls.check("http://host/path", android))
        // Desktop (no policy): any http host.
        assertEquals(ServerUrlCheck.Ok("http://192.168.1.5:8080"), ServerUrls.check("http://192.168.1.5:8080"))
        // Allowed, but warned about: http:// to anything but this machine (or the emulator's host alias).
        for (url in listOf("http://192.168.1.5:8080", "http://pod.example.com", "http://[2001:db8::1]", "http://10.0.2.20")) {
            assertTrue(ServerUrls.isCleartextRemote(url), url)
        }
        for (url in listOf(
            "https://pod.example.com", "http://localhost:8080", "http://LOCALHOST", "http://127.0.0.1:8080",
            "http://127.1.2.3", "http://[::1]:8080", "http://10.0.2.2:8080", "http://",
        )) {
            assertFalse(ServerUrls.isCleartextRemote(url), url)
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
