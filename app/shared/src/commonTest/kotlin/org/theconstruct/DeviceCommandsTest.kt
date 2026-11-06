package org.theconstruct

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonPrimitive
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The `action.device` commands of features `device_inbox_paging` (`load_older_notifications`, `mark_read`) and
 * `device_layout_controls` (`hide_layout`, `unhide_layout`, `delete_layout`), and pinned versions kept on the device.
 */
class DeviceCommandsTest {
    /** A pod with [count] notifications (n<count> newest), two layouts, and a pin store that outlives the controller. */
    private class PodBridge(
        count: Int = 0,
        val pins: MutableMap<String, StoredPin> = mutableMapOf(),
    ) : ConstructEngineBridge {
        var listener: ((EngineSnapshot) -> Unit)? = null
        val items = (count downTo 1).map { i ->
            AppNotification(id = "n%03d".format(i), appId = "c-a", channel = "c", title = "Note $i", createdAt = i.toLong())
        }.toMutableList()
        val hidden = mutableMapOf("c-a" to false, "c-b" to true)
        val deleted = mutableListOf<String>()
        val pages = mutableListOf<String?>()

        override fun subscribe(onState: (EngineSnapshot) -> Unit) {
            listener = onState
        }

        override fun dispatch(intentJson: String) {}

        override fun close() {}

        @Synchronized
        override fun listNotifications(before: String?, limit: Int, appId: String?): String {
            pages += before
            val start = before?.let { b -> items.indexOfFirst { it.id == b } + 1 } ?: 0
            val page = items.drop(start).take(limit)
            val next = if (start + page.size < items.size) ""","next_before":"${page.last().id}"""" else ""
            val list = page.joinToString(",") { ConstructJson.json.encodeToString(AppNotification.serializer(), it) }
            return """{"notifications":[$list],${unread()}$next}"""
        }

        @Synchronized
        override fun markNotificationsRead(ids: List<String>): String {
            items.replaceAll { if (it.id in ids) it.copy(read = true) else it }
            return "{${unread()}}"
        }

        private fun unread(): String {
            val n = items.count { !it.read }
            return """"unread_total":$n,"unread_by_app":{"c-a":$n}"""
        }

        @Synchronized
        override fun listApps(): String = """{"apps": [{"id": "home", "title": "Home"}""" +
            hidden.entries.joinToString("") { (id, h) -> """, {"id": "$id", "title": "$id", "kind": "custom", "hidden": $h}""" } +
            "]}"

        @Synchronized
        override fun setAppHidden(appId: String, hidden: Boolean) {
            this.hidden[appId] = hidden
        }

        @Synchronized
        override fun deleteCustomApp(appId: String) {
            if (appId !in hidden) throw LayoutApiException("No such layout.", LayoutApiException.Kind.NotFound, 404)
            hidden -= appId
            deleted += appId
        }

        override fun pinnedVersions(): List<StoredPin> = pins.values.toList()

        override fun setPinnedVersion(layoutId: String, version: Long?, specJson: String?) {
            if (version == null || specJson == null) pins -= layoutId else pins[layoutId] = StoredPin(layoutId, version, specJson)
        }

        fun emit(workspace: String = HOME) = listener!!(
            EngineSnapshot(
                workspace, "{}", true, 1, 0,
                authJson = """{"state":"signed_out","session":true}""",
                layoutJson = """{"app_id":"home","version":1}""",
            ),
        )
    }

    private fun device(c: ConstructController, unread: UnreadCounts = c.unread.value) = deviceData(
        c.state.value, unread, c.deviceApps.value, "desktop",
        DeviceSources(
            notifications = c.device.notifications,
            hasOlderNotifications = c.device.olderBefore != null,
            loadingNotifications = c.device.loadingOlder,
        ),
    )

    private suspend fun eventually(what: String, check: () -> Boolean) {
        repeat(200) {
            if (check()) return
            delay(10)
        }
        throw AssertionError("timed out waiting: $what")
    }

    @Test
    fun loadOlderAppendsAPageUntilTheEnd() = runBlocking {
        val bridge = PodBridge(count = 70)
        val c = ConstructController(bridge, HOME_APP, Dispatchers.Unconfined)
        c.device.loadNotifications()
        assertEquals(30, c.device.notifications.size)
        var inbox = device(c).entities["local://inbox"]!!
        assertEquals(JsonPrimitive(true), inbox["has_older"])
        assertEquals(JsonPrimitive(false), inbox["loading"])

        val older = ActionIntent.Device("load_older_notifications")
        c.dispatch(older)
        eventually("the second page") { c.device.notifications.size == 60 }
        assertEquals("n070", c.device.notifications.first().id)
        assertEquals("n011", c.device.notifications.last().id)
        assertEquals(60, device(c).queries["local:notifications"]!!.size)

        // A refresh keeps the older pages under the newest one.
        bridge.items.add(0, AppNotification(id = "n071", appId = "c-a", channel = "c", title = "New", createdAt = 71))
        c.device.loadNotifications()
        assertEquals(61, c.device.notifications.size)
        assertEquals("n071", c.device.notifications.first().id)

        c.dispatch(older)
        eventually("the last page") { c.device.olderBefore == null && !c.device.loadingOlder }
        assertEquals(71, c.device.notifications.size)
        assertEquals(71, c.device.notifications.map { it.id }.toSet().size, "no notification twice")
        inbox = device(c).entities["local://inbox"]!!
        assertEquals(JsonPrimitive(false), inbox["has_older"], "the button goes at the end")
        val asked = bridge.pages.size
        c.dispatch(older)
        assertEquals(asked, bridge.pages.size, "nothing more to ask for")
    }

    @Test
    fun aRefreshAfterNewArrivalsOffersOlderOnes() = runBlocking {
        val bridge = PodBridge(count = 6)
        val c = ConstructController(bridge, HOME_APP, Dispatchers.Unconfined)
        c.device.loadNotifications()
        assertNull(c.device.olderBefore, "everything fits one page")
        repeat(30) { i ->
            bridge.items.add(0, AppNotification(id = "n%03d".format(100 + i), appId = "c-a", channel = "c", title = "New $i", createdAt = 100L + i))
        }
        c.device.loadNotifications()
        assertEquals(30, c.device.notifications.size)
        assertEquals("n100", c.device.olderBefore, "the six older ones can be loaded")
    }

    @Test
    fun markReadClearsOneNotificationAndLowersTheUnreadCount() = runBlocking {
        val bridge = PodBridge(count = 3)
        val c = ConstructController(bridge, HOME_APP, Dispatchers.Unconfined)
        c.device.loadNotifications()
        c.dispatch(ActionIntent.Device("mark_read", id = "n002"))
        val row = device(c).entities["local://notifications/n002"]!!
        assertEquals(JsonPrimitive(false), row["unread"], "shown read at once")
        assertEquals(JsonPrimitive(true), device(c).entities["local://notifications/n001"]!!["unread"])
        eventually("the engine's unread count") { c.unread.value.total == 2 }
        assertEquals(JsonPrimitive(2), device(c).entities["local://inbox"]!!["unread"])
        assertTrue(bridge.items.first { it.id == "n002" }.read)
        c.dispatch(ActionIntent.Device("mark_read"))
        assertEquals(2, bridge.items.count { !it.read }, "no id: nothing")
    }

    @Test
    fun hideUnhideAndDeleteLayouts() = runBlocking {
        val bridge = PodBridge()
        val c = ConstructController(bridge, HOME_APP, Dispatchers.Unconfined)
        c.refreshDeviceApps()
        eventually("the layouts") { c.deviceApps.value.size == 3 }
        var d = device(c)
        assertEquals(listOf("local://layouts/c-a"), d.queries["local:layouts"])
        assertEquals(listOf("local://layouts/c-a", "local://layouts/c-b"), d.queries["local:all_layouts"])
        assertEquals(JsonPrimitive(true), d.entities["local://layouts/c-b"]!!["hidden"])
        assertEquals(JsonPrimitive(false), d.entities["local://layouts/c-a"]!!["hidden"])

        c.dispatch(ActionIntent.Device("hide_layout", app = "c-a"))
        assertEquals(emptyList(), device(c).queries["local:layouts"], "hidden at once")
        eventually("the pod hides it") { bridge.hidden["c-a"] == true }
        c.dispatch(ActionIntent.Device("unhide_layout", app = "c-b"))
        eventually("the pod shows it") { bridge.hidden["c-b"] == false }
        eventually("listed again") { device(c).queries["local:layouts"] == listOf("local://layouts/c-b") }

        c.navigation.present("c-b")
        c.dispatch(ActionIntent.Device("delete_layout", app = "c-b"))
        eventually("deleted") { bridge.deleted == listOf("c-b") && c.deviceApps.value.none { it.id == "c-b" } }
        assertNull(c.navigation.top, "a deleted layout leaves the stack")
        c.dispatch(ActionIntent.Device("delete_layout", app = HOME_APP))
        c.dispatch(ActionIntent.Device("hide_layout", app = HOME_APP))
        assertFalse(HOME_APP in bridge.deleted)
        c.dispatch(ActionIntent.Device("delete_layout", app = "c-missing"))
        eventually("the refusal is shown") { c.notice.value == "No such layout." }
    }

    @Test
    fun aPinnedVersionSurvivesARestart() {
        val store = mutableMapOf<String, StoredPin>()
        val first = ConstructController(PodBridge(pins = store), HOME_APP, Dispatchers.Unconfined)
        val pinned = ConstructJson.decodeNode(PINNED)
        assertTrue(first.pin("c-a", 4, pinned))
        assertTrue(first.pin(HOME_APP, 2, ConstructJson.decodeNode(HOME)))
        assertEquals(setOf("c-a", HOME_APP), store.keys)
        first.close()

        // The next run draws the pins before the engine's first snapshot.
        val bridge = PodBridge(pins = store)
        val second = ConstructController(bridge, HOME_APP, Dispatchers.Unconfined)
        assertEquals(mapOf("c-a" to 4L, HOME_APP to 2L), second.pins.value.mapValues { it.value.version })
        assertEquals(pinned, second.workspaceOf("c-a"))
        bridge.emit()
        assertEquals(pinned, second.workspaceOf("c-a"), "a snapshot doesn't replace a pin")
        val rows = deviceData(
            second.state.value, UnreadCounts(), listOf(AppSummary("c-a", "A", latestVersion = 5)), "desktop",
            DeviceSources(pins = second.pins.value.mapValues { it.value.version }),
        )
        assertEquals(JsonPrimitive("v4 · pinned"), rows.entities["local://layouts/c-a"]!!["version"])

        second.followLatest("c-a")
        assertEquals(setOf(HOME_APP), store.keys, "following the latest forgets the pin")
        val third = ConstructController(PodBridge(pins = store), HOME_APP, Dispatchers.Unconfined)
        assertEquals(setOf(HOME_APP), third.pins.value.keys)

        // An unreadable stored pin is dropped, not drawn.
        store["c-z"] = StoredPin("c-z", 1, "not json")
        val fourth = ConstructController(PodBridge(pins = store), HOME_APP, Dispatchers.Unconfined)
        assertNull(fourth.pins.value["c-z"])
    }

    private companion object {
        const val HOME = """{"type":"core://Workspace","id":"home","title":"Home","slots":[
          {"slot_id":"s","provider_uri":"p","content":{"type":"core://Text","value_expr":"Home","style":"Body"}}]}"""
        const val PINNED = """{"type":"core://Workspace","id":"c-a","title":"Old","slots":[
          {"slot_id":"s","provider_uri":"p","content":{"type":"core://Text","value_expr":"v4","style":"Body"}}]}"""
    }
}
