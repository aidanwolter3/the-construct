package org.theconstruct

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.int
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

private class FakeBridge : ConstructEngineBridge {
    var listener: ((EngineSnapshot) -> Unit)? = null
    val dispatched = mutableListOf<String>()
    private var nextSeq = 1L

    override fun subscribe(onState: (EngineSnapshot) -> Unit) {
        listener = onState
    }

    override fun dispatch(intentJson: String) {
        dispatched += intentJson
    }

    var logouts = 0
    var closes = 0

    override fun close() {
        closes++
    }

    override fun logout() {
        logouts++
    }

    /** Emits with an auto-incremented seq unless [seq] is given. */
    fun emit(workspace: String, entities: String, online: Boolean?, pending: Int = 0, seq: Long = nextSeq++) =
        listener!!(EngineSnapshot(workspace, entities, online, seq, pending))

    /** Emits a snapshot whose `layout_json` is [layout]. */
    fun emitLayout(workspace: String, entities: String, layout: String) =
        listener!!(EngineSnapshot(workspace, entities, true, nextSeq++, 0, layoutJson = layout))
}

class ConstructControllerTest {
    /** Splits a bundle into the engine snapshot shape: layout node JSON + entities JSON. */
    private fun snapshotOf(bundle: String): Pair<String, String> {
        val obj = Json.parseToJsonElement(bundle).jsonObject
        return obj["workspace"].toString() to obj["entities"].toString()
    }

    @Test
    fun decodesCounterSnapshotAndForwardsDispatch() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge)
        val (ws, ents) = snapshotOf(COUNTER_BUNDLE_JSON)
        bridge.emit(ws, ents, true)

        val s = controller.state.value
        assertNull(s.error)
        assertEquals(true, s.isOnline)
        val workspace = assertIs<UiNode.Workspace>(s.workspace)
        assertEquals("Home", workspace.title)
        assertEquals(0, s.entities["core://counters/home"]!!["count"]!!.jsonPrimitive.int)
        assertEquals("Count: 0", interpolate("Count: \${core://counters/home#count}", s.entities))

        val container = workspace.slots[0].content as UiNode.Container
        val buttons = container.children[2] as UiNode.Container
        val increment = buttons.children[0] as UiNode.Button
        controller.dispatch(increment.onClick)

        assertEquals(1, bridge.dispatched.size)
        val json = bridge.dispatched[0]
        assertTrue(json.contains("\"action.mutation.optimistic\""), json)
        assertTrue(json.contains("core://counters/home"), json)
        assertEquals(increment.onClick, ConstructJson.decodeIntent(json))
    }

    @Test
    fun logoutIsForwardedAndFakesDefaultToNoAuthRedirect() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge)
        controller.logout()
        assertEquals(1, bridge.logouts)
        assertEquals(false, bridge.handleAuthRedirect("construct://oauth?code=x&state=y"))
    }

    @Test
    fun nullWorkspaceMeansLoadingWithoutError() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge)
        bridge.emit("null", "{}", false)
        val s = controller.state.value
        assertNull(s.workspace)
        assertNull(s.error)
        assertEquals(false, s.isOnline)
        assertTrue(s.entities.isEmpty())
    }

    @Test
    fun malformedSnapshotSetsErrorAndKeepsLastGoodState() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge)
        val (ws, ents) = snapshotOf(COUNTER_BUNDLE_JSON)
        bridge.emit(ws, ents, true)
        bridge.emit("{not json", ents, false)
        val s = controller.state.value
        assertNotNull(s.error)
        assertIs<UiNode.Workspace>(s.workspace)
        assertEquals(false, s.isOnline)
    }

    @Test
    fun unknownNodeTypesDecodeWithoutError() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge)
        bridge.emit("""{"type":"future://Widget","x":1}""", "{}", true)
        val s = controller.state.value
        assertNull(s.error)
        assertEquals("future://Widget", assertIs<UiNode.Unknown>(s.workspace).type)
    }

    @Test
    fun staleSnapshotsAreDropped() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge)
        val (ws, _) = snapshotOf(COUNTER_BUNDLE_JSON)
        bridge.emit(ws, COUNT_3, true, pending = 0, seq = 10)
        bridge.emit(ws, COUNT_1, false, pending = 2, seq = 9) // older: dropped
        bridge.emit(ws, COUNT_1, false, pending = 2, seq = 10) // same seq: dropped
        var s = controller.state.value
        assertEquals(10, s.seq)
        assertEquals(3, s.entities["core://counters/home"]!!["count"]!!.jsonPrimitive.int)
        assertEquals(true, s.isOnline)
        assertEquals(0, s.pendingActions)

        bridge.emit(ws, COUNT_1, false, pending = 2, seq = 11)
        s = controller.state.value
        assertEquals(11, s.seq)
        assertEquals(1, s.entities["core://counters/home"]!!["count"]!!.jsonPrimitive.int)
        assertEquals(2, s.pendingActions)
    }

    @Test
    fun staleMalformedSnapshotDoesNotSetError() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge)
        val (ws, ents) = snapshotOf(COUNTER_BUNDLE_JSON)
        bridge.emit(ws, ents, true, seq = 5)
        bridge.emit("{not json", ents, false, seq = 4)
        assertNull(controller.state.value.error)
        assertEquals(true, controller.state.value.isOnline)
    }

    @Test
    fun pendingCountFollowsSnapshots() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge)
        val (ws, ents) = snapshotOf(COUNTER_BUNDLE_JSON)
        assertEquals(0, controller.state.value.pendingActions)
        bridge.emit(ws, ents, false, pending = 1)
        assertEquals(1, controller.state.value.pendingActions)
        bridge.emit(ws, ents, false, pending = 3)
        assertEquals(3, controller.state.value.pendingActions)
        // A decode failure still carries connectivity and the pending count.
        bridge.emit("{not json", ents, true, pending = 2)
        assertNotNull(controller.state.value.error)
        assertEquals(2, controller.state.value.pendingActions)
        bridge.emit(ws, ents, true, pending = 0)
        assertNull(controller.state.value.error)
        assertEquals(0, controller.state.value.pendingActions)
    }

    @Test
    fun openAppSwapsEnginesAndIgnoresTheOldOne() {
        val first = FakeBridge()
        val second = FakeBridge()
        val opened = mutableListOf<String>()
        val controller = ConstructController(first, "pixelfed_home") { id -> opened += id; second }
        val (ws, ents) = snapshotOf(COUNTER_BUNDLE_JSON)
        first.emit(ws, ents, true, seq = 40)
        controller.pin("pixelfed_home", 1, UiNode.Text("old", TextStyle.Body))
        assertTrue(controller.openApp("pixelfed_home"), "already on screen")
        assertEquals(emptyList(), opened)

        assertTrue(controller.openApp("home"))
        assertEquals(listOf("home"), opened)
        assertEquals(1, first.closes)
        assertEquals("home", controller.currentAppId())
        assertNull(controller.pinned.value, "a pin belongs to its app")
        assertNull(controller.state.value.workspace, "Loading... until the new engine's first snapshot")
        // The new engine's seq starts again; the old engine's late snapshots are dropped.
        second.emit(ws, COUNT_1, true, seq = 1)
        first.emit(ws, COUNT_3, false, seq = 41)
        assertEquals(1, controller.state.value.entities["core://counters/home"]!!["count"]!!.jsonPrimitive.int)
        assertEquals(true, controller.state.value.isOnline)
        controller.dispatch(ActionIntent.NavigateBack)
        assertEquals(1, second.dispatched.size)
        assertEquals(0, first.dispatched.size)
    }

    @Test
    fun connectivityIsUnknownUntilTheFirstSnapshotAndCarriesAcrossOpenApp() {
        val first = FakeBridge()
        val second = FakeBridge()
        val controller = ConstructController(first, "pixelfed_home") { second }
        assertNull(controller.state.value.isOnline, "unknown at cold start")
        assertFalse(showMenuBadge(controller.state.value, null), "no offline badge while unknown")
        val (ws, ents) = snapshotOf(COUNTER_BUNDLE_JSON)
        first.emit(ws, ents, false)
        assertEquals(false, controller.state.value.isOnline)
        assertTrue(showMenuBadge(controller.state.value, null))
        first.emit(ws, ents, true)
        assertTrue(controller.openApp("home"))
        assertEquals(true, controller.state.value.isOnline, "same server: connectivity carries over until the new engine reports")
        assertFalse(showMenuBadge(controller.state.value, null))
        // The new engine's cached snapshot comes before its first fetch: still unknown, so no offline flash.
        second.emit(ws, ents, null, seq = 1)
        assertEquals(true, controller.state.value.isOnline)
        assertFalse(showMenuBadge(controller.state.value, null), "no badge while the new engine connects")
        second.emit(ws, ents, false, seq = 2)
        assertEquals(false, controller.state.value.isOnline)
    }

    @Test
    fun unknownConnectivityAtColdStartShowsNoBadge() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge, "pixelfed_home")
        val (ws, ents) = snapshotOf(COUNTER_BUNDLE_JSON)
        bridge.emit(ws, ents, null)
        assertNotNull(controller.state.value.workspace, "the cached layout shows at once")
        assertNull(controller.state.value.isOnline)
        assertFalse(showMenuBadge(controller.state.value, null))
    }

    @Test
    fun openAppAndPinAfterCloseDoNothing() {
        val first = FakeBridge()
        var created = 0
        val controller = ConstructController(first, "pixelfed_home") { created++; FakeBridge() }
        controller.close()
        controller.close()
        assertEquals(1, first.closes, "close is idempotent")
        assertTrue(controller.isClosed)
        assertFalse(controller.openApp("home"))
        assertFalse(controller.openApp("pixelfed_home"), "not even the app on screen")
        assertEquals(0, created, "no engine is created after close")
        assertEquals("pixelfed_home", controller.currentAppId())
        assertFalse(controller.pin("pixelfed_home", 1, UiNode.Text("old", TextStyle.Body)))
        assertNull(controller.pinned.value)
    }

    @Test
    fun anEngineCreatedWhileClosingIsClosed() {
        val first = FakeBridge()
        val next = FakeBridge()
        lateinit var controller: ConstructController
        // The UI goes away while the factory runs (here: re-entrantly, from the factory itself).
        controller = ConstructController(first, "pixelfed_home") { controller.close(); next }
        assertFalse(controller.openApp("home"))
        assertEquals(1, first.closes)
        assertEquals(1, next.closes, "the engine created meanwhile is closed, not leaked")
        assertEquals("pixelfed_home", controller.currentAppId())
        assertNull(next.listener, "never subscribed: its snapshots can't reach the UI")
    }

    @Test
    fun openAppWithoutAFactoryChangesNothing() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge, "pixelfed_home")
        assertFalse(controller.canOpenApps)
        assertFalse(controller.openApp("home"))
        assertEquals("pixelfed_home", controller.currentAppId())
        assertEquals(0, bridge.closes)
        // A factory that throws is reported the same way.
        val failing = ConstructController(bridge, "pixelfed_home") { throw IllegalStateException("no engine") }
        assertFalse(failing.openApp("home"))
        assertEquals("pixelfed_home", failing.currentAppId())
    }

    @Test
    fun appIdFallsBackToTheSnapshot() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge)
        assertNull(controller.currentAppId())
        bridge.listener!!(EngineSnapshot("null", "{}", true, 1, 0, layoutJson = """{"app_id":"pixelfed_home","version":7}"""))
        assertEquals("pixelfed_home", controller.currentAppId())
        assertEquals(ActiveLayout("pixelfed_home", 7), controller.state.value.activeLayout)
    }

    private companion object {
        const val COUNT_1 = """{"core://counters/home":{"count":1}}"""
        const val COUNT_3 = """{"core://counters/home":{"count":3}}"""
    }

    @Test
    fun aDeletedAppOnScreenFallsBackToTheDefaultAppWithANotice() {
        val first = FakeBridge()
        val second = FakeBridge()
        val opened = mutableListOf<String>()
        val controller = ConstructController(first, "c-kitchen2dm", kotlinx.coroutines.Dispatchers.Unconfined, "pixelfed_home") { id ->
            opened += id
            second
        }
        val (ws, ents) = snapshotOf(COUNTER_BUNDLE_JSON)
        first.emitLayout(ws, ents, """{"app_id":"c-kitchen2dm","version":3}""")
        assertNull(controller.notice.value)
        first.emitLayout(ws, ents, """{"app_id":"c-kitchen2dm","gone":true}""")
        // (The background dispatcher is Unconfined here: the switch already happened.)
        assertEquals(listOf("pixelfed_home"), opened)
        assertFalse(controller.state.value.activeLayout.gone, "the new engine's state")
        assertEquals("pixelfed_home", controller.currentAppId())
        assertEquals(APP_DELETED, controller.notice.value, "the notice outlives the switch")
        assertEquals(1, first.closes)
        controller.dismissNotice()
        assertNull(controller.notice.value)
        // Without a default (or when the default itself is gone): stay, with the notice.
        val lone = FakeBridge()
        val stay = ConstructController(lone, "pixelfed_home", kotlinx.coroutines.Dispatchers.Unconfined, "pixelfed_home") { error("no switch") }
        lone.emitLayout(ws, ents, """{"app_id":"pixelfed_home","gone":true}""")
        assertTrue(stay.state.value.activeLayout.gone)
        assertEquals(APP_DELETED, stay.notice.value)
        assertEquals("pixelfed_home", stay.currentAppId())
    }
}
