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
    var listener: ((String, String, Boolean) -> Unit)? = null
    val dispatched = mutableListOf<String>()

    override fun subscribe(onState: (workspaceJson: String, entitiesJson: String, isOnline: Boolean) -> Unit) {
        listener = onState
    }

    override fun dispatch(intentJson: String) {
        dispatched += intentJson
    }

    fun emit(workspace: String, entities: String, online: Boolean) = listener!!(workspace, entities, online)
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
}
