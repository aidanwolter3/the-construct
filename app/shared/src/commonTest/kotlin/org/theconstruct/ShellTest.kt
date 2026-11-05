package org.theconstruct

import kotlinx.coroutines.Dispatchers
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** The layout-first shell (decision `blueprints-and-home`): its vocabulary, device data and controller. */
class ShellTest {
    private class Bridge(val app: String) : ConstructEngineBridge {
        var listener: ((EngineSnapshot) -> Unit)? = null
        val dispatched = mutableListOf<String>()
        var forgotten = 0
        var closed = 0
        private var seq = 1L

        override fun subscribe(onState: (EngineSnapshot) -> Unit) {
            listener = onState
        }

        override fun dispatch(intentJson: String) {
            dispatched += intentJson
        }

        override fun forgetSession() {
            forgotten++
        }

        override fun close() {
            closed++
        }

        override fun listApps(): String =
            """{"apps": [{"id": "home", "title": "Home"}, {"id": "c-aaaaaaaaaa", "title": "Kitchen", "kind": "custom"}]}"""

        fun emit(workspace: String, session: Boolean = true) = listener!!(
            EngineSnapshot(
                workspace, "{}", true, seq++, 0,
                authJson = """{"state":"signed_out","session":$session}""",
                layoutJson = """{"app_id":"$app","version":1}""",
            ),
        )
    }

    private val dockJson = """
        {"type": "core://Workspace", "id": "home", "title": "Home", "slots": [{"slot_id": "dock", "provider_uri": "local://apps",
          "content": {"type": "core://Dock", "field": "tab", "query_key": "local:apps", "max_items": 4,
            "item": {"value": "${'$'}item.id", "label": "${'$'}item.title", "badge": "${'$'}item.unread",
                     "content": {"type": "core://AppFrame", "app": "${'$'}item.id"}},
            "after": [{"value": "more", "label": "More", "icon": "more",
              "content": {"type": "core://Button", "label": "Settings",
                "on_click": {"intent": "action.device", "command": "open_settings"}}}]}}]}
    """.trimIndent()

    @Test
    fun dockFrameAndDeviceIntentRoundTrip() {
        val ws = assertIs<UiNode.Workspace>(ConstructJson.decodeNode(dockJson))
        assertTrue(fillsScreen(ws))
        val dock = assertIs<UiNode.Dock>(ws.slots[0].content)
        assertEquals("local:apps", dock.queryKey)
        assertEquals(4, dock.maxItems)
        assertIs<UiNode.AppFrame>(dock.item!!.content)
        val button = assertIs<UiNode.Button>(dock.after[0].content)
        assertEquals(ActionIntent.Device("open_settings"), button.onClick)
        // Re-encodes to what the pod sent.
        assertEquals(ws, ConstructJson.decodeNode(ConstructJson.json.encodeToString(UiNode.serializer(), ws)))
        // `open_app` binds `$item.id` like any intent.
        val bound = bindIntent(ActionIntent.Device("open_app", "${'$'}item.id"), buildJsonObject { put("id", "c-x") })
        assertEquals(ActionIntent.Device("open_app", "c-x"), bound)
        val toggle = bindIntent(
            ActionIntent.Device("toggle_channel", "${'$'}item.app_id", "${'$'}item.id"),
            buildJsonObject { put("app_id", "c-x"); put("id", "likes") },
        )
        assertEquals(ActionIntent.Device("toggle_channel", "c-x", "likes"), toggle)
    }

    @Test
    fun deviceDataListsTheAppsButHomeAndTheConnection() {
        val apps = listOf(
            AppSummary("home", "Home", custom = true),
            AppSummary("c-a", "Kitchen", icon = "recipe", latestVersion = 3, custom = true),
            AppSummary("c-b", "Hidden", custom = true, hidden = true),
        )
        val state = ConstructState(isOnline = false, pendingActions = 2)
        val d = deviceData(state, UnreadCounts(5, mapOf("c-a" to 2)), apps, "desktop")
        assertEquals(listOf("local://apps/c-a"), d.queries["local:apps"])
        val kitchen = d.entities["local://apps/c-a"]!!
        assertEquals(JsonPrimitive("Kitchen"), kitchen["title"])
        assertEquals(JsonPrimitive(2), kitchen["unread"])
        assertEquals(JsonPrimitive("v3"), kitchen["detail"])
        assertEquals(JsonPrimitive(1), d.entities["local://apps"]!!["count"])
        assertEquals(JsonPrimitive("Offline · 2 pending"), d.entities["local://connection"]!!["label"])
        assertEquals(JsonPrimitive(true), d.entities["local://connection"]!!["attention"])
        assertEquals(JsonPrimitive("5 unread"), d.entities["local://inbox"]!!["label"])
        assertEquals(JsonPrimitive("desktop"), d.entities["local://device"]!!["platform"])
        // A Conditional can read it: the "No apps yet" welcome shows only with none.
        assertFalse(conditionShows("\${local://apps#count}", "0", null, null, d.entities))
        assertTrue(conditionShows("\${local://apps#count}", null, null, null, d.entities))
    }

    @Test
    fun deviceDataListsNotificationsChannelsSignInsAndServers() {
        val now = 1_800_000_000L
        val sources = DeviceSources(
            notifications = listOf(AppNotification("n1", "c-a", "likes", "Dana liked your post", createdAt = now - 120)),
            channels = listOf(NotificationChannelInfo("c-a", "likes", "Likes", enabled = false)),
            signIns = listOf(SignInSummary("pixelfed", "Pixelfed", "https://pixelfed.example", "dana")),
            providers = listOf(
                SignInProvider("pixelfed", "Pixelfed", intent = null),
                SignInProvider("other", "Other", intent = ActionIntent.AuthLogin("https://x.example", "/a", "/t")),
            ),
            server = ServerChoice("http://127.0.0.1:8080", listOf(ServerOption("local", "Local", "http://127.0.0.1:8080/"))) {},
            nowSeconds = now,
        )
        val apps = listOf(AppSummary("c-a", "Photos", custom = true))
        val d = deviceData(ConstructState(), UnreadCounts(), apps, "desktop", sources)
        val n = d.entities[d.queries["local:notifications"]!!.single()]!!
        assertEquals(JsonPrimitive("Photos"), n["app_title"])
        assertEquals(JsonPrimitive("2 min"), n["when"])
        assertEquals(JsonPrimitive("Today"), n["day"])
        assertEquals(JsonPrimitive(true), n["unread"])
        assertEquals(JsonPrimitive(false), d.entities["local://channels/c-a/likes"]!!["enabled"])
        assertEquals(JsonPrimitive("@dana · pixelfed.example"), d.entities["local://sign_ins/pixelfed"]!!["detail"])
        assertEquals(listOf("local://providers/other"), d.queries["local:providers"], "signed in or no intent: not offered")
        assertEquals(JsonPrimitive(true), d.entities["local://servers/local"]!!["selected"])
        assertEquals(JsonPrimitive("http://127.0.0.1:8080"), d.entities["local://session"]!!["server"])
    }

    @Test
    fun theShellShowsBootstrapThenHomeThenRescue() {
        val home = ShellSnapshot(UiNode.Spacer())
        assertEquals(ShellView.Loading, shellView(ConstructState(), null, onHome = true))
        val signedOut = ConstructState(seq = 1, auth = AuthInfo(session = false))
        assertEquals(ShellView.Bootstrap, shellView(signedOut, home, onHome = true))
        val open = ConstructState(seq = 1, auth = AuthInfo(session = true))
        assertEquals(ShellView.Home(home), shellView(open, home, onHome = false))
        assertEquals(ShellView.Rescue, shellView(open.copy(error = "409"), null, onHome = true))
        assertEquals(ShellView.Loading, shellView(open, null, onHome = false))
        // The bundled layouts parse.
        assertIs<UiNode.Workspace>(BundledLayouts.bootstrap)
        assertIs<UiNode.Workspace>(BundledLayouts.rescue)
    }

    @Test
    fun shellModeKeepsHomeAndRoutesDeviceActions() {
        val homeBridge = Bridge(HOME_APP)
        val opened = mutableListOf<String>()
        val controller = ConstructController(homeBridge, HOME_APP, Dispatchers.Unconfined) { id ->
            opened += id
            Bridge(id)
        }
        assertTrue(controller.shellMode)
        homeBridge.emit(dockJson)
        val shell = controller.shell.value
        assertIs<UiNode.Workspace>(shell?.workspace)
        assertEquals(1L, shell?.version)

        // The frame's app takes the engine; the home layout stays.
        assertTrue(controller.openApp("c-aaaaaaaaaa"))
        assertEquals(listOf("c-aaaaaaaaaa"), opened)
        assertEquals(shell, controller.shell.value)

        // open_app goes to the dock; nothing reaches the engine.
        controller.dispatchShell(ActionIntent.Device("open_app", "c-bbbbbbbbbb"))
        assertEquals("c-bbbbbbbbbb", controller.dockRequest.value)
        controller.dockRequestHandled()
        assertNull(controller.dockRequest.value)
        controller.dispatchShell(ActionIntent.Device("open_settings"))
        assertTrue(controller.menu.open && controller.menu.settingsOpen)
        controller.dispatchShell(ActionIntent.Device("no_such_command"))
        assertTrue(homeBridge.dispatched.isEmpty())

        // Leaving the pod forgets the home layout and goes back to home for the next pod.
        controller.dispatchShell(ActionIntent.Device("leave_pod"))
        assertNull(controller.shell.value)
        assertEquals(listOf("c-aaaaaaaaaa", HOME_APP), opened)
    }

    @Test
    fun outsideShellModeOpenAppSwitchesTheEngine() {
        val opened = mutableListOf<String>()
        val controller = ConstructController(Bridge("a"), "a", Dispatchers.Unconfined) { id ->
            opened += id
            Bridge(id)
        }
        assertFalse(controller.shellMode)
        controller.dispatch(ActionIntent.Device("open_app", "b"))
        assertEquals(listOf("b"), opened)
        assertNull(controller.dockRequest.value)
    }
}
