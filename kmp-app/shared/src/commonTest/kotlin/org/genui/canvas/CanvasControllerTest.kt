package org.genui.canvas

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.int
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

private class FakeBridge : CanvasEngineBridge {
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

    override fun logout() {
        logouts++
    }

    /** Emits with an auto-incremented seq unless [seq] is given. */
    fun emit(workspace: String, entities: String, online: Boolean, pending: Int = 0, seq: Long = nextSeq++) =
        listener!!(EngineSnapshot(workspace, entities, online, seq, pending))
}

class CanvasControllerTest {
    /** Splits a bundle into the engine snapshot shape: layout node JSON + entities JSON. */
    private fun snapshotOf(bundle: String): Pair<String, String> {
        val obj = Json.parseToJsonElement(bundle).jsonObject
        return obj["workspace"].toString() to obj["entities"].toString()
    }

    @Test
    fun decodesCounterSnapshotAndForwardsDispatch() {
        val bridge = FakeBridge()
        val controller = CanvasController(bridge)
        val (ws, ents) = snapshotOf(COUNTER_BUNDLE_JSON)
        bridge.emit(ws, ents, true)

        val s = controller.state.value
        assertNull(s.error)
        assertTrue(s.isOnline)
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
        assertEquals(increment.onClick, CanvasJson.decodeIntent(json))
    }

    @Test
    fun logoutIsForwardedAndFakesDefaultToNoAuthRedirect() {
        val bridge = FakeBridge()
        val controller = CanvasController(bridge)
        controller.logout()
        assertEquals(1, bridge.logouts)
        assertEquals(false, bridge.handleAuthRedirect("canvas://oauth?code=x&state=y"))
    }

    @Test
    fun nullWorkspaceMeansLoadingWithoutError() {
        val bridge = FakeBridge()
        val controller = CanvasController(bridge)
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
        val controller = CanvasController(bridge)
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
        val controller = CanvasController(bridge)
        bridge.emit("""{"type":"future://Widget","x":1}""", "{}", true)
        val s = controller.state.value
        assertNull(s.error)
        assertEquals("future://Widget", assertIs<UiNode.Unknown>(s.workspace).type)
    }

    @Test
    fun staleSnapshotsAreDropped() {
        val bridge = FakeBridge()
        val controller = CanvasController(bridge)
        val (ws, _) = snapshotOf(COUNTER_BUNDLE_JSON)
        bridge.emit(ws, COUNT_3, true, pending = 0, seq = 10)
        bridge.emit(ws, COUNT_1, false, pending = 2, seq = 9) // older: dropped
        bridge.emit(ws, COUNT_1, false, pending = 2, seq = 10) // same seq: dropped
        var s = controller.state.value
        assertEquals(10, s.seq)
        assertEquals(3, s.entities["core://counters/home"]!!["count"]!!.jsonPrimitive.int)
        assertTrue(s.isOnline)
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
        val controller = CanvasController(bridge)
        val (ws, ents) = snapshotOf(COUNTER_BUNDLE_JSON)
        bridge.emit(ws, ents, true, seq = 5)
        bridge.emit("{not json", ents, false, seq = 4)
        assertNull(controller.state.value.error)
        assertTrue(controller.state.value.isOnline)
    }

    @Test
    fun pendingCountFollowsSnapshots() {
        val bridge = FakeBridge()
        val controller = CanvasController(bridge)
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

    private companion object {
        const val COUNT_1 = """{"core://counters/home":{"count":1}}"""
        const val COUNT_3 = """{"core://counters/home":{"count":3}}"""
    }
}
