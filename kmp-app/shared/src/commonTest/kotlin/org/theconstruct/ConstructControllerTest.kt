package org.theconstruct

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

    override fun logout() {
        logouts++
    }

    /** Emits with an auto-incremented seq unless [seq] is given. */
    fun emit(workspace: String, entities: String, online: Boolean, pending: Int = 0, seq: Long = nextSeq++) =
        listener!!(EngineSnapshot(workspace, entities, online, seq, pending))
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
        val controller = ConstructController(bridge)
        val (ws, ents) = snapshotOf(COUNTER_BUNDLE_JSON)
        bridge.emit(ws, ents, true, seq = 5)
        bridge.emit("{not json", ents, false, seq = 4)
        assertNull(controller.state.value.error)
        assertTrue(controller.state.value.isOnline)
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
    fun agentStatusFollowsSnapshotsAndPromptDispatchesTheAgentIntent() {
        val bridge = FakeBridge()
        val controller = ConstructController(bridge)
        val (ws, ents) = snapshotOf(COUNTER_BUNDLE_JSON)
        bridge.emit(ws, ents, true)
        assertEquals(AgentInfo(), controller.state.value.agent) // default agentJson is idle
        bridge.listener!!(
            EngineSnapshot(ws, ents, true, 50, 0, agentJson = """{"state":"done","provider":"rules","layout_version":3}"""),
        )
        assertEquals(AgentInfo("done", null, "rules", 3), controller.state.value.agent)
        // A decode failure still carries the agent status.
        bridge.listener!!(EngineSnapshot("{not json", ents, true, 51, 0, agentJson = """{"state":"running"}"""))
        assertNotNull(controller.state.value.error)
        assertEquals("running", controller.state.value.agent.state)

        controller.dispatch(ActionIntent.PromptAgent("Hide captions"))
        assertEquals("""{"intent":"action.agent.prompt","prompt_text":"Hide captions"}""", bridge.dispatched.last())
    }

    @Test
    fun agentInfoParsingAndStatusText() {
        assertEquals(AgentInfo(), AgentInfo.parse("{not json"))
        assertEquals(AgentInfo(), AgentInfo.parse("""{"state":null}"""))
        assertEquals(
            AgentInfo("error", "unsupported prompt", null, null),
            AgentInfo.parse("""{"state":"error","message":"unsupported prompt","layout_version":"x"}"""),
        )
        assertNull(agentStatusText(AgentInfo()))
        assertEquals("Applying...", agentStatusText(AgentInfo("running")))
        assertEquals("Layout updated (v3, rules)", agentStatusText(AgentInfo("done", provider = "rules", layoutVersion = 3)))
        assertEquals("Layout updated", agentStatusText(AgentInfo("done")))
        assertEquals("unsupported prompt", agentStatusText(AgentInfo("error", "unsupported prompt")))
        assertEquals("The layout agent failed", agentStatusText(AgentInfo("error")))
        // `unchanged: true` (the pod found nothing to change): the engine's message, not "Layout updated".
        val unchanged = AgentInfo.parse(
            """{"state":"done","message":"No change needed (v2)","provider":"rules","layout_version":2,"unchanged":true}""",
        )
        assertEquals(AgentInfo("done", "No change needed (v2)", "rules", 2, unchanged = true), unchanged)
        assertEquals("No change needed (v2)", agentStatusText(unchanged))
        assertEquals("No change needed (v4)", agentStatusText(AgentInfo("done", layoutVersion = 4, unchanged = true)))
        assertEquals(false, AgentInfo.parse("""{"state":"done","unchanged":"yes"}""").unchanged)
    }

    @Test
    fun postCardSlotKeysAreUniquePerKind() {
        val text = UiNode.Text("x", TextStyle.Body)
        val header = UiNode.AuthorHeader("\$item.author_uri")
        assertEquals(
            listOf("AuthorHeader#0", "Text#0", "Text#1"),
            postCardSlotKeys(listOf(header, text, text)).map { it.first },
        )
        // The same node keeps its key when it moves to another slot.
        assertEquals("AuthorHeader#0", postCardSlotKeys(listOf(text, header)).last().first)
    }

    private companion object {
        const val COUNT_1 = """{"core://counters/home":{"count":1}}"""
        const val COUNT_3 = """{"core://counters/home":{"count":3}}"""
    }
}
