package org.theconstruct

import kotlinx.coroutines.Dispatchers
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
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
    fun emit(workspace: String = WORKSPACE, unread: String = NO_UNREAD, version: Long? = 1) {
        val layout = if (version == null) """{"app_id":"$appId"}""" else """{"app_id":"$appId","version":$version}"""
        listener!!(EngineSnapshot(workspace, "{}", true, seq++, 0, layoutJson = layout, notificationsJson = unread))
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
        val controller = ConstructController(bridge, "a", Dispatchers.Unconfined, null)
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
        val c = ConstructController(plain, null, Dispatchers.Unconfined, null)
        assertTrue(c.notificationChannels().isEmpty())
        assertNull(c.pushPublicKey())
        assertNull(c.handlePushMessage("{}"))
    }

    @Test
    fun liveNotificationsReachTheHookFromEveryEngineAndDeviceInfoCarriesOver() {
        val first = NotifyingBridge("a")
        val second = NotifyingBridge("b")
        val controller = ConstructController(first, "a", Dispatchers.Unconfined) { second }
        controller.setDeviceInfo("desktop", "host")
        assertEquals(listOf("desktop" to "host"), first.deviceInfo)
        val heard = mutableListOf<AppNotification>()
        controller.onNotification = { heard += it }
        first.live!!(NOTIFICATION)
        first.live!!("not json") // dropped, never thrown into the engine
        assertEquals(listOf("n1"), heard.map { it.id })

        assertTrue(controller.openApp("b"))
        assertEquals(listOf("desktop" to "host"), second.deviceInfo, "applied to engines created later")
        second.live!!(NOTIFICATION.replace("n1", "n2"))
        first.live!!(NOTIFICATION.replace("n1", "n3")) // the replaced engine: ignored
        assertEquals(listOf("n1", "n2"), heard.map { it.id })

        // A throwing hook doesn't escape either.
        controller.onNotification = { throw IllegalStateException("boom") }
        second.live!!(NOTIFICATION)
    }

    @Test
    fun openNotificationRoutesInTheAppOnScreen() {
        val bridge = NotifyingBridge("a")
        val controller = ConstructController(bridge, "a", Dispatchers.Unconfined, null)
        bridge.emit()
        controller.openNotification("a", "x://posts/7/comments", "n1")
        assertEquals("comments", controller.navigation.top?.screen?.id)
        assertEquals(mapOf("id" to "7"), controller.navigation.top?.params)
        assertEquals(listOf(listOf("n1")), bridge.markedRead)
        assertEquals(UnreadCounts(1, mapOf("a" to 1)), controller.unread.value, "the mark-read answer updates the count")

        // No target, or one no screen matches: the app stays as it is.
        controller.navigation.clear()
        controller.openNotification("a", null, null)
        controller.openNotification("a", "x://users/7", "n2")
        assertNull(controller.navigation.top)
        assertEquals(listOf(listOf("n1"), listOf("n2")), bridge.markedRead)
        assertEquals(0, bridge.closes)
    }

    @Test
    fun openNotificationOpensAnotherAppAndWaitsForItsWorkspace() {
        val first = NotifyingBridge("a")
        val second = NotifyingBridge("b")
        val opened = mutableListOf<String>()
        // Cold start: the notification is opened before any snapshot arrived.
        val controller = ConstructController(first, "a", Dispatchers.Unconfined) { id -> opened += id; second }
        controller.openNotification("b", "x://posts/9/comments", "n9")
        assertEquals(listOf("b"), opened)
        assertEquals("b", controller.currentAppId())
        assertEquals(1, first.closes)
        assertNull(controller.navigation.top, "waits for the app's workspace")
        // The signed-out screen first (unversioned, no screens): still waiting.
        second.emit(PLAIN_WORKSPACE, version = null)
        assertNull(controller.navigation.top)
        second.emit()
        assertEquals("comments", controller.navigation.top?.screen?.id)
        assertEquals(mapOf("id" to "9"), controller.navigation.top?.params)
        // Applied once: later snapshots don't push it again.
        second.emit()
        assertEquals(1, controller.navigation.stack.size)
        assertEquals(listOf(listOf("n9")), first.markedRead, "marked read on the engine current when tapped")
    }

    @Test
    fun aTargetTheAppCannotShowIsDroppedAndAnotherAppSwitchCancelsIt() {
        val bridges = mutableMapOf<String, NotifyingBridge>()
        val controller = ConstructController(NotifyingBridge("a"), "a", Dispatchers.Unconfined) { id ->
            NotifyingBridge(id).also { bridges[id] = it }
        }
        controller.openNotification("b", "x://nowhere/1", null)
        bridges.getValue("b").emit()
        assertNull(controller.navigation.top)

        controller.openNotification("c", "x://posts/1/comments", null)
        assertTrue(controller.openApp("d"), "the user switches apps before c loaded")
        bridges.getValue("c").emit()
        bridges.getValue("d").emit()
        assertNull(controller.navigation.top, "the pending target belonged to c")
    }

    @Test
    fun pushAndChannelHelpersNeverThrow() {
        val bridge = NotifyingBridge("a")
        val controller = ConstructController(bridge, "a", Dispatchers.Unconfined, null)
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
