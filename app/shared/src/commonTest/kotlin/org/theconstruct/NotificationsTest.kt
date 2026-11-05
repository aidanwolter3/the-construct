package org.theconstruct

import kotlinx.coroutines.Dispatchers
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** A bridge with notifications: records calls, emits snapshots and live arrivals on demand. */
private class NotifyingBridge(val appId: String) : ConstructEngineBridge {
    var listener: ((EngineSnapshot) -> Unit)? = null
    var live: ((String) -> Unit)? = null
    val dispatched = mutableListOf<String>()
    val markedRead = mutableListOf<List<String>>()
    val deviceInfo = mutableListOf<Pair<String, String>>()
    var pushRegistrations = 0
    var closes = 0
    val openedLayouts = mutableListOf<String>()

    override fun openLayout(id: String) {
        openedLayouts += id
    }
    private var seq = 1L

    override fun subscribe(onState: (EngineSnapshot) -> Unit) {
        listener = onState
    }

    override fun dispatch(intentJson: String) {
        dispatched += intentJson
    }

    override fun close() {
        closes++
    }

    override fun setNotificationListener(onNotification: (String) -> Unit) {
        live = onNotification
    }

    override fun setDeviceInfo(platform: String, name: String) {
        deviceInfo += platform to name
    }

    override fun markNotificationsRead(ids: List<String>): String {
        markedRead += ids
        return """{"unread_total":1,"unread_by_app":{"$appId":1}}"""
    }

    override fun notificationChannels(): String =
        """{"channels":[{"app_id":"$appId","id":"likes","title":"Likes","description":"d","default_enabled":true,"importance":"default","enabled":false}]}"""

    override fun registerPush(endpoint: String, p256dh: String, auth: String) {
        pushRegistrations++
        throw LayoutApiException("no push here")
    }

    override fun handlePushMessage(payloadJson: String): String? = payloadJson.takeIf { it.contains("\"id\"") }

    /** A snapshot of [workspace] (versioned unless [version] is null) with [unread] notifications. */
    fun emit(workspace: String = WORKSPACE, unread: String = NO_UNREAD, version: Long? = 1, layouts: String = "{}") {
        val layout = if (version == null) """{"app_id":"$appId"}""" else """{"app_id":"$appId","version":$version}"""
        listener!!(EngineSnapshot(workspace, "{}", true, seq++, 0, layoutJson = layout, notificationsJson = unread, layoutsJson = layouts))
    }

    companion object {
        const val NO_UNREAD = """{"unread_total":0,"unread_by_app":{}}"""
    }
}

private const val WORKSPACE = """
{"type":"core://Workspace","id":"app","title":"App","slots":[
  {"slot_id":"s","provider_uri":"p","content":{"type":"core://Text","value_expr":"Home","style":"Body"}}],
 "screens":[{"id":"comments","route":"x://posts/{id}/comments","title":"Comments",
   "content":{"type":"core://Text","value_expr":"Comments","style":"Body"}}]}
"""

/** A screen without screens (e.g. a signed-out screen). */
private const val PLAIN_WORKSPACE = """
{"type":"core://Workspace","id":"app","title":"Sign in","slots":[
  {"slot_id":"s","provider_uri":"p","content":{"type":"core://Text","value_expr":"Sign in","style":"Body"}}]}
"""

private const val NOTIFICATION = """{"id":"n1","app_id":"a","channel":"likes","title":"@bob liked your post",
  "body":"Sunrise","target_uri":"x://posts/7/comments","group_key":"7:like","created_at":1790000000,"read":false,"future":1}"""

class NotificationsTest {
    @Test
    fun parsesNotificationsLeniently() {
        val n = assertNotNull(NotificationsJson.parseNotification(NOTIFICATION))
        assertEquals(
            AppNotification(
                id = "n1", appId = "a", channel = "likes", title = "@bob liked your post", body = "Sunrise",
                targetUri = "x://posts/7/comments", groupKey = "7:like", createdAt = 1_790_000_000, read = false,
            ),
            n,
        )
        // Optional fields may be absent; a notification without an id or app is dropped.
        val minimal = assertNotNull(NotificationsJson.parseNotification("""{"id":"n2","app_id":"a","channel":"c","title":"t","created_at":5}"""))
        assertEquals("", minimal.body)
        assertNull(minimal.targetUri)
        assertFalse(minimal.read)
        assertNull(NotificationsJson.parseNotification("""{"id":"","app_id":"a","channel":"c","title":"t","created_at":5}"""))
        assertNull(NotificationsJson.parseNotification("not json"))

        val page = NotificationsJson.parsePage(
            """{"notifications":[$NOTIFICATION,{"id":"bad"},7],"unread_total":3,"unread_by_app":{"a":2,"b":1},"next_before":"n1"}""",
        )
        assertEquals(listOf("n1"), page.notifications.map { it.id })
        assertEquals(UnreadCounts(3, mapOf("a" to 2, "b" to 1)), page.unread)
        assertEquals("n1", page.nextBefore)
        assertNull(NotificationsJson.parsePage("""{"notifications":[]}""").nextBefore)
    }

    @Test
    fun parsesUnreadCountsAndChannels() {
        assertEquals(UnreadCounts(2, mapOf("a" to 2)), NotificationsJson.parseUnread("""{"unread_total":2,"unread_by_app":{"a":2}}"""))
        assertEquals(UnreadCounts(), NotificationsJson.parseUnread("garbage"))
        // A missing total is the per-app sum.
        assertEquals(3, NotificationsJson.parseUnread("""{"unread_by_app":{"a":2,"b":1}}""").total)

        val channels = NotificationsJson.parseChannels(
            """{"channels":[{"app_id":"a","id":"likes","title":"Likes","description":"When…","default_enabled":true,
              "importance":"high","enabled":false},{"app_id":"a","id":"other","title":"Other","default_enabled":false},{"id":"x"}]}""",
        )
        assertEquals(2, channels.size)
        assertEquals(NotificationChannelInfo("a", "likes", "Likes", "When…", true, "high", false), channels[0])
        assertFalse(channels[1].enabled, "enabled defaults to default_enabled")
        assertEquals("default", channels[1].importance)
        assertEquals("a.likes", NotificationChannels.androidId("a", "likes"))
    }

    @Test
    fun timeLabelsAndDayGroups() {
        val now = 1_790_000_000L
        assertEquals("now", notificationTimeLabel(now - 5, now))
        assertEquals("3 min", notificationTimeLabel(now - 180, now))
        assertEquals("2 h", notificationTimeLabel(now - 7_300, now))
        assertEquals("now", notificationTimeLabel(now + 60, now), "a clock ahead of ours reads as now")
        assertEquals("Today", notificationDayLabel(now, now))
        assertEquals("Yesterday", notificationDayLabel(now - 86_400, now))
        assertEquals("", unreadBadgeText(0))
        assertEquals("7", unreadBadgeText(7))
        assertEquals("99+", unreadBadgeText(120))

        fun n(id: String, at: Long) = AppNotification(id = id, appId = "a", channel = "c", title = id, createdAt = at)
        val groups = groupNotificationsByDay(listOf(n("1", now), n("2", now - 60), n("3", now - 86_400), n("4", now - 10 * 86_400)), now)
        assertEquals(listOf("Today", "Yesterday"), groups.take(2).map { it.first })
        assertEquals(listOf("1", "2"), groups[0].second.map { it.id })
        assertEquals(3, groups.size)
        assertNull(notificationImageUrl("/x", null), "a pod path needs the base URL")
        assertEquals("https://pod/x", notificationImageUrl("/x", "https://pod/"))
        assertEquals("https://cdn/x", notificationImageUrl("https://cdn/x", "https://pod"))
    }

    @Test
    fun unreadFollowsSnapshotsAndFakesWithoutNotificationsReadZero() {
        val bridge = NotifyingBridge("a")
        val controller = ConstructController(bridge, "a", Dispatchers.Unconfined)
        assertEquals(0, controller.unread.value.total)
        bridge.emit(unread = """{"unread_total":4,"unread_by_app":{"a":3,"b":1}}""")
        assertEquals(UnreadCounts(4, mapOf("a" to 3, "b" to 1)), controller.unread.value)

        // The interface defaults: listing throws a readable error, the rest do nothing.
        val plain = object : ConstructEngineBridge {
            override fun subscribe(onState: (EngineSnapshot) -> Unit) {}
            override fun dispatch(intentJson: String) {}
        }
        val e = runCatching { plain.listNotifications() }.exceptionOrNull()
        assertEquals("Notifications aren't available here.", e?.message)
        val c = ConstructController(plain, null, Dispatchers.Unconfined)
        assertTrue(c.notificationChannels().isEmpty())
        assertNull(c.pushPublicKey())
        assertNull(c.handlePushMessage("{}"))
    }

    @Test
    fun liveNotificationsReachTheHook() {
        val bridge = NotifyingBridge("a")
        val controller = ConstructController(bridge, "a", Dispatchers.Unconfined)
        controller.setDeviceInfo("desktop", "host")
        assertEquals(listOf("desktop" to "host"), bridge.deviceInfo)
        val heard = mutableListOf<AppNotification>()
        controller.onNotification = { heard += it }
        bridge.live!!(NOTIFICATION)
        bridge.live!!("not json") // dropped, never thrown into the engine
        assertEquals(listOf("n1"), heard.map { it.id })
        // A throwing hook doesn't escape either.
        controller.onNotification = { throw IllegalStateException("boom") }
        bridge.live!!(NOTIFICATION)
    }

    @Test
    fun openNotificationRoutesInTheRoot() {
        val bridge = NotifyingBridge("a")
        val controller = ConstructController(bridge, "a", Dispatchers.Unconfined)
        bridge.emit()
        controller.openNotification("a", "x://posts/7/comments", "n1")
        assertEquals("comments", controller.navigation.topScreen?.screen?.id)
        assertEquals(mapOf("id" to "7"), controller.navigation.topScreen?.params)
        assertEquals(listOf(listOf("n1")), bridge.markedRead)
        assertEquals(UnreadCounts(1, mapOf("a" to 1)), controller.unread.value, "the mark-read answer updates the count")

        // No target, or one no screen matches: nothing opens.
        controller.navigation.clear()
        controller.openNotification("a", null, null)
        controller.openNotification("a", "x://users/7", "n2")
        assertNull(controller.navigation.top)
        assertEquals(listOf(listOf("n1"), listOf("n2")), bridge.markedRead)
    }

    @Test
    fun openNotificationPresentsItsLayoutAndWaitsForIt() {
        val bridge = NotifyingBridge("home")
        // Cold start: the notification is opened before any snapshot arrived.
        val controller = ConstructController(bridge, "home", Dispatchers.Unconfined)
        controller.openNotification("pixelfed", "x://posts/9/comments", "n9", layout = "c-photos")
        assertEquals(listOf("c-photos"), bridge.openedLayouts, "the same engine loads it")
        assertEquals(0, bridge.closes)
        val presented = assertIs<LayoutEntry>(controller.navigation.top)
        assertEquals("c-photos", presented.layoutId)
        // Its screen waits for the layout.
        bridge.emit(PLAIN_WORKSPACE)
        assertEquals(1, controller.navigation.stack.size)
        bridge.emit(PLAIN_WORKSPACE, layouts = """{"c-photos":{"workspace":$WORKSPACE,"version":2,"gone":false}}""")
        val screen = assertNotNull(controller.navigation.topScreen)
        assertEquals("comments", screen.screen.id)
        assertEquals("c-photos", screen.layoutId)
        assertEquals(mapOf("id" to "9"), screen.params)
        // Applied once.
        bridge.emit(PLAIN_WORKSPACE, layouts = """{"c-photos":{"workspace":$WORKSPACE,"version":2,"gone":false}}""")
        assertEquals(2, controller.navigation.stack.size)
        assertEquals(listOf(listOf("n9")), bridge.markedRead)
        // Back pops the screen, then the layout.
        assertTrue(controller.navigation.back())
        assertTrue(controller.navigation.back())
        assertNull(controller.navigation.top)
        // Without a layout (an older pod), the blueprint's id is the layout.
        controller.openNotification("pixelfed", null, null)
        assertEquals("pixelfed", controller.navigation.top?.layoutId)
    }

    @Test
    fun aTargetIsDroppedWhenItsLayoutIsClosedOrGone() {
        val bridge = NotifyingBridge("home")
        val controller = ConstructController(bridge, "home", Dispatchers.Unconfined)
        bridge.emit(PLAIN_WORKSPACE)
        controller.openNotification("b", "x://posts/1/comments", null, layout = "b")
        controller.navigation.back() // closed before it loaded
        bridge.emit(PLAIN_WORKSPACE, layouts = """{"b":{"workspace":$WORKSPACE,"version":1,"gone":false}}""")
        assertNull(controller.navigation.top, "the pending target belonged to the closed layout")
        controller.openNotification("c", "x://posts/1/comments", null, layout = "c")
        bridge.emit(PLAIN_WORKSPACE, layouts = """{"c":{"gone":true}}""")
        assertEquals(1, controller.navigation.stack.size, "only the layout, which says it's gone")
    }

    @Test
    fun pushAndChannelHelpersNeverThrow() {
        val bridge = NotifyingBridge("a")
        val controller = ConstructController(bridge, "a", Dispatchers.Unconfined)
        controller.registerPush("https://push.example/x", "k", "a") // the bridge throws: logged
        assertEquals(1, bridge.pushRegistrations)
        assertEquals(listOf("likes"), controller.notificationChannels().map { it.id })
        assertEquals("n1", controller.handlePushMessage(NOTIFICATION)?.id)
        assertNull(controller.handlePushMessage("{}"))
        assertNull(controller.appTitle("a"), "the catalog hasn't loaded")
        controller.close()
        controller.openNotification("a", "x://posts/1/comments", "n1")
        assertTrue(bridge.markedRead.isEmpty(), "nothing after close")
    }
}
