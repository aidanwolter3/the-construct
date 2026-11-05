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
    val openedLayouts = mutableListOf<String>()

    override fun openLayout(id: String) {
        openedLayouts += id
    }

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
    fun oneEngineServesEveryLayout() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge, "home")
        val (ws, ents) = snapshotOf(COUNTER_BUNDLE_JSON)
        bridge.emit(ws, ents, true, seq = 1)
        // Presenting another layout asks the same engine for it; nothing is closed or swapped.
        controller.dispatch(ActionIntent.Navigate(presentation = PresentationMode.Push, layout = "c-other"))
        assertEquals(listOf("c-other"), bridge.openedLayouts)
        assertEquals(0, bridge.closes)
        assertEquals("c-other", controller.navigation.currentLayout())
        assertNull(controller.workspaceOf("c-other"), "spinner until the engine has it")
        bridge.listener!!(
            EngineSnapshot(
                ws, ents, true, 2, 0,
                layoutsJson = """{"c-other":{"workspace":{"type":"core://Workspace","id":"c-other","title":"Other","slots":[]},"version":4,"gone":false}}""",
            ),
        )
        assertEquals("Other", (controller.workspaceOf("c-other") as UiNode.Workspace).title)
        assertEquals(4, controller.state.value.layouts["c-other"]!!.version)
        // A pin is per layout.
        controller.pin("c-other", 1, UiNode.Text("old", TextStyle.Body))
        assertIs<UiNode.Text>(controller.workspaceOf("c-other"))
        assertIs<UiNode.Workspace>(controller.workspaceOf("home"))
        controller.followLatest("c-other")
        assertIs<UiNode.Workspace>(controller.workspaceOf("c-other"))
        // System Back pops the presented layout.
        assertTrue(controller.navigation.back())
        assertEquals("home", controller.navigation.currentLayout())
        assertFalse(controller.navigation.back(), "nothing left: the platform's own Back")
    }

    @Test
    fun connectivityIsUnknownUntilTheFirstSnapshot() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge, "pixelfed")
        assertNull(controller.state.value.isOnline, "unknown at cold start")
        assertFalse(showMenuBadge(controller.state.value, null), "no offline badge while unknown")
        val (ws, ents) = snapshotOf(COUNTER_BUNDLE_JSON)
        bridge.emit(ws, ents, false)
        assertEquals(false, controller.state.value.isOnline)
        assertTrue(showMenuBadge(controller.state.value, null))
        // A later snapshot that doesn't know keeps the last answer.
        bridge.emit(ws, ents, null)
        assertEquals(false, controller.state.value.isOnline)
    }

    @Test
    fun unknownConnectivityAtColdStartShowsNoBadge() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge, "pixelfed")
        val (ws, ents) = snapshotOf(COUNTER_BUNDLE_JSON)
        bridge.emit(ws, ents, null)
        assertNotNull(controller.state.value.workspace, "the cached layout shows at once")
        assertNull(controller.state.value.isOnline)
        assertFalse(showMenuBadge(controller.state.value, null))
    }

    @Test
    fun pinAndOpenLayoutAfterCloseDoNothing() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge, "pixelfed")
        controller.close()
        controller.close()
        assertEquals(1, bridge.closes, "close is idempotent")
        assertTrue(controller.isClosed)
        controller.openLayout("c-other")
        assertEquals(emptyList(), bridge.openedLayouts)
        assertFalse(controller.pin("pixelfed", 1, UiNode.Text("old", TextStyle.Body)))
        assertTrue(controller.pins.value.isEmpty())
    }

    @Test
    fun appIdFallsBackToTheSnapshot() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge)
        assertEquals(HOME_APP, controller.currentAppId(), "home until the engine says")
        bridge.listener!!(EngineSnapshot("null", "{}", true, 1, 0, layoutJson = """{"app_id":"pixelfed","version":7}"""))
        assertEquals("pixelfed", controller.currentAppId())
        assertEquals(ActiveLayout("pixelfed", 7), controller.state.value.activeLayout)
    }

    private companion object {
        const val COUNT_1 = """{"core://counters/home":{"count":1}}"""
        const val COUNT_3 = """{"core://counters/home":{"count":3}}"""
    }

    @Test
    fun aDeletedRootSaysSo() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge, "c-kitchen2dm")
        val (ws, ents) = snapshotOf(COUNTER_BUNDLE_JSON)
        bridge.emitLayout(ws, ents, """{"app_id":"c-kitchen2dm","version":3}""")
        assertNull(controller.notice.value)
        bridge.emitLayout(ws, ents, """{"app_id":"c-kitchen2dm","gone":true}""")
        assertTrue(controller.state.value.activeLayout.gone)
        assertEquals(APP_DELETED, controller.notice.value)
        assertEquals(0, bridge.closes, "the engine stays")
        controller.dismissNotice()
        assertNull(controller.notice.value)
    }
}
