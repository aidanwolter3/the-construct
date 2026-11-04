package org.theconstruct

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonPrimitive
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** Records snapshots' listener and dispatches; [opened] counts engines a factory made from it. */
private class LauncherBridge(val app: String) : ConstructEngineBridge {
    var listener: ((EngineSnapshot) -> Unit)? = null
    val dispatched = mutableListOf<String>()
    var closed = false
    private var seq = 1L

    override fun subscribe(onState: (EngineSnapshot) -> Unit) {
        listener = onState
    }

    override fun dispatch(intentJson: String) {
        dispatched += intentJson
    }

    override fun close() {
        closed = true
    }

    override fun logout() {}

    fun emit(workspace: String, entities: String = "{}", queries: String = "{}") =
        listener!!(EngineSnapshot(workspace, entities, true, seq++, 0, queriesJson = queries))
}

private const val HOME_LAYOUT = """{"type":"core://Workspace","id":"launcher","title":"Home","chrome":"hidden",
 "slots":[{"slot_id":"home","provider_uri":"core://home","content":{"type":"core://Swipe",
  "on_swipe_up":{"intent":"action.navigation.open","target_uri":"home://drawer","presentation":"Push"},
  "content":{"type":"core://Backdrop","tilt_deg":30,"cell_dp":40,"grid_color":"#7CC8DA",
   "content":{"type":"core://Column","children":[{"type":"core://Clock","format":"time"},{"type":"core://Clock","format":"date"},
     {"type":"core://Tappable","on_click":{"intent":"action.app.open","app_id":"${'$'}item.target"},
      "on_long_press":{"intent":"action.device.launch","package":"${'$'}item.target"},
      "content":{"type":"core://Text","value_expr":"${'$'}item.title","style":"Headline"}},
     {"type":"core://OnAppear","intent":{"intent":"action.shell.open","panel":"inbox"}}]},
   "footer":{"type":"core://TextField","field":"q","autofocus":true}}}}],
 "screens":[{"id":"drawer","route":"home://drawer","title":"Apps","chrome":"hidden","content":{"type":"core://Spacer"}}]}"""

class LauncherTest {
    @Test
    fun decodesTheLauncherVocabulary() {
        val ws = assertIs<UiNode.Workspace>(ConstructJson.decodeNode(HOME_LAYOUT))
        assertEquals(Chrome.Hidden, ws.chrome)
        assertEquals(Chrome.Hidden, ws.screens.single().chrome)
        val swipe = assertIs<UiNode.Swipe>(ws.slots.single().content)
        assertEquals(ActionIntent.Navigate("home://drawer", PresentationMode.Push), swipe.onSwipeUp)
        assertNull(swipe.onSwipeDown)
        val backdrop = assertIs<UiNode.Backdrop>(swipe.content)
        assertEquals(30, backdrop.tiltDeg)
        assertEquals(40, backdrop.cellDp)
        assertEquals("#7CC8DA", backdrop.gridColor)
        val column = assertIs<UiNode.Column>(backdrop.content)
        assertEquals(UiNode.Clock(ClockFormat.Time), column.children[0])
        assertEquals(UiNode.Clock(ClockFormat.Date), column.children[1])
        val tap = assertIs<UiNode.Tappable>(column.children[2])
        assertEquals(ActionIntent.OpenApp("\$item.target"), tap.onClick)
        assertEquals(ActionIntent.LaunchDeviceApp("\$item.target"), tap.onLongPress)
        assertEquals(UiNode.OnAppear(ActionIntent.OpenShell(ShellPanel.Inbox)), column.children[3])
        assertTrue(assertIs<UiNode.TextField>(backdrop.footer).autofocus)
    }

    @Test
    fun defaultsAreLeftOffTheWire() {
        val ws = UiNode.Workspace("a", "A", emptyList())
        assertFalse(ConstructJson.encodeNode(ws).contains("chrome"))
        assertEquals("""{"type":"core://Clock"}""", ConstructJson.encodeNode(UiNode.Clock()))
        assertEquals(
            """{"intent":"action.app.open","app_id":"recipes"}""",
            ConstructJson.encodeIntent(ActionIntent.OpenApp("recipes")),
        )
        assertEquals(
            """{"intent":"action.device.launch","package":"com.x"}""",
            ConstructJson.encodeIntent(ActionIntent.LaunchDeviceApp("com.x")),
        )
        assertEquals(
            """{"intent":"action.shell.open","panel":"menu"}""",
            ConstructJson.encodeIntent(ActionIntent.OpenShell(ShellPanel.Menu)),
        )
    }

    @Test
    fun bindsItemTokensInTheNewIntents() {
        val item = JsonObject(mapOf("id" to JsonPrimitive("recipes"), "package" to JsonPrimitive("com.android.camera2")))
        assertEquals(ActionIntent.OpenApp("recipes"), bindIntent(ActionIntent.OpenApp("\$item.id"), item))
        assertEquals(
            ActionIntent.LaunchDeviceApp("com.android.camera2"),
            bindIntent(ActionIntent.LaunchDeviceApp("\$item.package"), item),
        )
        assertEquals(ActionIntent.OpenShell(ShellPanel.Menu), bindIntent(ActionIntent.OpenShell(ShellPanel.Menu), item))
    }

    @Test
    fun theGridCrowdsTowardsTheTopAndConverges() {
        val lines = perspectiveGrid(390f, 844f, 36f, 36, 700f)
        val horizontal = lines.filter { it[1] == it[3] }.map { it[1] }.sortedDescending()
        assertEquals(844f, horizontal.first())
        // Rows get closer together further up the floor.
        val gaps = horizontal.zipWithNext { a, b -> a - b }
        assertTrue(gaps.first() > gaps.last(), "$gaps")
        // Vertical lines lean towards the centre at the top.
        val vertical = lines.filter { it[1] != it[3] }
        assertTrue(vertical.isNotEmpty())
        val right = vertical.first { it[0] > 195f + 36f }
        assertTrue(right[2] < right[0], right.toList().toString())
        // No tilt: a flat, even grid.
        val flat = perspectiveGrid(390f, 844f, 36f, 0, 700f).filter { it[1] == it[3] }.map { it[1] }.sortedDescending()
        assertTrue(flat.zipWithNext { a, b -> a - b }.all { kotlin.math.abs(it - 36f) < 0.01f })
        assertTrue(perspectiveGrid(0f, 0f, 36f, 36, 700f).isEmpty())
    }

    @Test
    fun deviceAppsAreMergedSortedAndReplaced() {
        val s = mergeDeviceApps(
            ConstructState(entities = mapOf("x://1" to JsonObject(emptyMap()))),
            listOf(DeviceApp("com.b", "camera"), DeviceApp("com.a", "Atlas"), DeviceApp("com.n", "9Gag"), DeviceApp("", "Bad")),
        )
        assertEquals(listOf("device://apps/com.n", "device://apps/com.a", "device://apps/com.b"), s.queries[DEVICE_APPS_QUERY])
        val camera = s.entities.getValue("device://apps/com.b")
        assertEquals("C", camera["letter"]!!.jsonPrimitive.content)
        assertEquals("device", camera["kind"]!!.jsonPrimitive.content)
        assertEquals("#", s.entities.getValue("device://apps/com.n")["letter"]!!.jsonPrimitive.content)
        assertTrue("x://1" in s.entities)
        val again = mergeDeviceApps(s, listOf(DeviceApp("com.z", "Zoo")))
        assertEquals(listOf("device://apps/com.z"), again.queries[DEVICE_APPS_QUERY])
        assertFalse("device://apps/com.a" in again.entities)
    }

    @Test
    fun theControllerListsDeviceAppsInEveryState() {
        val bridge = LauncherBridge("launcher")
        val controller = ConstructController(bridge, "launcher")
        controller.setDeviceApps(listOf(DeviceApp("com.android.camera2", "Camera")))
        assertEquals(listOf("device://apps/com.android.camera2"), controller.state.value.queries[DEVICE_APPS_QUERY])
        bridge.emit(HOME_LAYOUT, queries = """{"home:pins":[]}""")
        val s = controller.state.value
        assertEquals(listOf("device://apps/com.android.camera2"), s.queries[DEVICE_APPS_QUERY])
        assertEquals(emptyList(), s.queries["home:pins"])
        assertNotNull(s.entities["device://apps/com.android.camera2"])
    }

    @Test
    fun launcherIntentsAreHandledByTheClient() {
        val bridges = mutableListOf<LauncherBridge>()
        val home = LauncherBridge("launcher")
        val controller = ConstructController(home, "launcher", homeAppId = "launcher", openBridge = { id -> LauncherBridge(id).also(bridges::add) })
        val launched = mutableListOf<String>()
        controller.deviceLauncher = { launched += it }
        home.emit(HOME_LAYOUT)

        controller.dispatch(ActionIntent.LaunchDeviceApp("com.android.camera2"))
        assertEquals(listOf("com.android.camera2"), launched)
        assertEquals(listOf("com.android.camera2"), controller.launchedApps.value)
        // An unbound binding or a blank package does nothing.
        controller.dispatch(ActionIntent.LaunchDeviceApp("\$item.package"))
        controller.dispatch(ActionIntent.OpenApp(" "))
        assertEquals(1, launched.size)

        controller.dispatch(ActionIntent.OpenShell(ShellPanel.Inbox))
        assertTrue(controller.inbox.open)
        controller.dispatch(ActionIntent.OpenApp("recipes"))
        assertFalse(controller.inbox.open)
        assertEquals("recipes", controller.currentAppId())
        assertTrue(home.closed)
        assertTrue(home.dispatched.isEmpty(), "nothing reaches the engine: ${home.dispatched}")

        // Back at another app's root: home again.
        assertTrue(controller.goHome())
        assertEquals("launcher", controller.currentAppId())
        assertFalse(controller.goHome())
        controller.dispatch(ActionIntent.OpenShell(ShellPanel.Menu))
        assertTrue(controller.menu.open)
    }

    @Test
    fun theBundledLayoutIsDrawnUntilTheEngineHasOne() {
        val bundled = assertIs<UiNode.Workspace>(BundledLayouts.layout("launcher"))
        assertEquals("launcher", bundled.id)
        assertEquals(Chrome.Hidden, bundled.chrome)
        assertNotNull(BundledLayouts.number("launcher"))
        assertNull(BundledLayouts.layout("no_such_app"))

        val bridge = LauncherBridge("launcher")
        val controller = ConstructController(
            bridge, "launcher", homeAppId = "launcher", fallbackLayouts = BundledLayouts.forApps("launcher", null, "no_such_app"),
        )
        controller.setDeviceApps(listOf(DeviceApp("com.x", "X")))
        // At once, before any snapshot: the bundled home, with the device's apps.
        var s = controller.state.value
        assertTrue(s.fallback)
        assertEquals(bundled, s.workspace)
        assertEquals(listOf("device://apps/com.x"), s.queries[DEVICE_APPS_QUERY])
        // The engine has nothing yet (first start offline): still the bundled one.
        bridge.emit("null")
        s = controller.state.value
        assertTrue(s.fallback)
        assertEquals(bundled, s.workspace)
        // Screens of the bundled layout open.
        assertTrue(controller.navigation.canOpen("home://drawer"))
        // Intents still go to the engine (they queue in its outbox).
        controller.dispatch(ActionIntent.LoadData("/v1/home/screens/request?id=a"))
        assertEquals(1, bridge.dispatched.size)
        // The engine's workspace wins as soon as there is one.
        bridge.emit(HOME_LAYOUT)
        s = controller.state.value
        assertFalse(s.fallback)
        assertEquals(ConstructJson.decodeNode(HOME_LAYOUT), s.workspace)
        assertEquals(listOf("device://apps/com.x"), s.queries[DEVICE_APPS_QUERY])
    }

    @Test
    fun appsWithoutABundledLayoutStillShowLoading() {
        val bridge = LauncherBridge("recipes")
        val controller = ConstructController(bridge, "recipes", fallbackLayouts = BundledLayouts.forApps("launcher"))
        bridge.emit("null")
        assertNull(controller.state.value.workspace)
        assertFalse(controller.state.value.fallback)
    }

    @Test
    fun darkSystemBarsFollowTheHiddenChrome() {
        assertTrue(ConstructState(workspace = ConstructJson.decodeNode(HOME_LAYOUT)).prefersDarkSystemBars())
        assertFalse(ConstructState(workspace = UiNode.Workspace("a", "A", emptyList())).prefersDarkSystemBars())
        assertFalse(ConstructState().prefersDarkSystemBars())
    }
}
