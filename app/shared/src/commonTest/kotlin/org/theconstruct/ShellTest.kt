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

/** Home and the layouts it embeds and presents (decisions `blueprints-and-home`, `layouts-not-apps`). */
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

        override fun signIns(): String =
            """{"sign_ins": [], "providers": [{"id": "pixelfed", "title": "Pixelfed", "server_placeholder": "pixelfed.social",
              "intent": {"intent": "action.auth.login", "instance_url": "https://default.example",
                         "app_path": "/v1/auth/pixelfed/app", "token_path": "/v1/auth/pixelfed/token"}}]}"""

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

    private val homeJson = """
        {"type": "core://Workspace", "id": "home", "title": "Home", "slots": [
          {"slot_id": "card", "provider_uri": "local://home",
           "content": {"type": "core://Layout", "layout": "c-card", "height_dp": 120}},
          {"slot_id": "sheet", "provider_uri": "local://home",
           "content": {"type": "core://Layout", "layout": "inbox", "presentation": "Sheet", "show_when": "${'$'}form.inbox",
                       "animation": "sheet_up", "params": {"tab": "all"}}},
          {"slot_id": "list", "provider_uri": "local://layouts",
           "content": {"type": "core://List", "query_key": "local:layouts", "item_template": {"type": "core://Tappable",
             "on_click": {"intent": "action.navigation.open", "layout": "${'$'}item.id", "presentation": "Push"},
             "content": {"type": "core://Text", "style": "Body", "value_expr": "${'$'}item.title"}}}}]}
    """.trimIndent()

    @Test
    fun layoutNodesAndLayoutNavigationRoundTrip() {
        val ws = assertIs<UiNode.Workspace>(ConstructJson.decodeNode(homeJson))
        val card = assertIs<UiNode.Layout>(ws.slots[0].content)
        assertEquals(EmbedPresentation.Inline, card.presentation)
        assertEquals(120, card.heightDp)
        val sheet = assertIs<UiNode.Layout>(ws.slots[1].content)
        assertEquals(EmbedPresentation.Sheet, sheet.presentation)
        assertEquals(Animation.SheetUp, sheet.animation)
        assertEquals(mapOf("tab" to "all"), sheet.params)
        // Re-encodes to what the pod sent.
        assertEquals(ws, ConstructJson.decodeNode(ConstructJson.json.encodeToString(UiNode.serializer(), ws)))
        // `layout` and `params` bind `$item` like any intent field.
        val list = assertIs<UiNode.ItemList>(ws.slots[2].content)
        val open = assertIs<UiNode.Tappable>(list.itemTemplate).onClick
        val bound = bindIntent(open, buildJsonObject { put("id", "c-x") })
        assertEquals(ActionIntent.Navigate(presentation = PresentationMode.Push, layout = "c-x"), bound)
        val toggle = bindIntent(
            ActionIntent.Device("toggle_channel", "${'$'}item.app_id", "${'$'}item.id"),
            buildJsonObject { put("app_id", "c-x"); put("id", "likes") },
        )
        assertEquals(ActionIntent.Device("toggle_channel", "c-x", "likes"), toggle)
        // A plain screen navigation encodes as before.
        assertEquals(
            """{"intent":"action.navigation.open","target_uri":"x://a","presentation":"Push"}""",
            ConstructJson.encodeIntent(ActionIntent.Navigate("x://a", PresentationMode.Push)),
        )
    }

    @Test
    fun routeParamsBindInAPresentedLayout() {
        val ws = ConstructJson.decodeNode(
            """{"type": "core://Workspace", "id": "x", "title": "Post ${'$'}route.id", "slots": []}""",
        )
        assertEquals("Post 7", (bindRouteParams(ws, mapOf("id" to "7")) as UiNode.Workspace).title)
        assertEquals(ws, bindRouteParams(ws, emptyMap()))
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
        assertEquals(listOf("local://layouts/c-a"), d.queries["local:layouts"])
        assertEquals(listOf("local://apps/c-a"), d.queries["local:apps"], "the old name, for older layouts")
        assertEquals(JsonPrimitive(1), d.entities["local://layouts"]!!["count"])
        val kitchen = d.entities["local://apps/c-a"]!!
        assertEquals(JsonPrimitive("Kitchen"), kitchen["title"])
        assertEquals(JsonPrimitive(2), kitchen["unread"])
        assertEquals(JsonPrimitive("v3"), kitchen["detail"])
        assertEquals(JsonPrimitive(1), d.entities["local://apps"]!!["count"])
        assertEquals(JsonPrimitive("Offline · 2 pending"), d.entities["local://connection"]!!["label"])
        assertEquals(JsonPrimitive(true), d.entities["local://connection"]!!["attention"])
        assertEquals(JsonPrimitive("5 unread"), d.entities["local://inbox"]!!["label"])
        assertEquals(JsonPrimitive("desktop"), d.entities["local://device"]!!["platform"])
        assertEquals(JsonPrimitive(""), d.entities["local://device"]!!["notification_permission"], "none on desktop")
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
    fun deviceDataListsTheVersionsOfOneApp() {
        val history = VersionHistory(
            versions = listOf(AppVersionInfo(2, note = "A plain list"), AppVersionInfo(1, note = "Created")),
            loaded = true,
        )
        val apps = listOf(AppSummary("c-a", "Recipes", latestVersion = 2, custom = true))
        val following = deviceData(ConstructState(), UnreadCounts(), apps, "desktop",
            DeviceSources(versionsApp = "c-a", versions = history))
        assertEquals(listOf("local://versions/c-a/2", "local://versions/c-a/1"), following.queries["local:versions"])
        assertEquals(JsonPrimitive(true), following.entities["local://versions/c-a/2"]!!["showing"])
        assertEquals(JsonPrimitive("v2 · latest"), following.entities["local://apps/c-a"]!!["version"])
        val pinned = deviceData(ConstructState(), UnreadCounts(), apps, "desktop",
            DeviceSources(versionsApp = "c-a", versions = history, pins = mapOf("c-a" to 1L)))
        assertEquals(JsonPrimitive(true), pinned.entities["local://versions/c-a/1"]!!["showing"])
        assertEquals(JsonPrimitive("1"), pinned.entities["local://versions"]!!["pinned"])
        assertEquals(JsonPrimitive("v1 · pinned"), pinned.entities["local://apps/c-a"]!!["version"])
        // A Conditional's `equals` may name the item.
        assertTrue(conditionShows("\$form.page", "\$item.id", buildJsonObject { put("id", "c-a") }, mapOf("page" to "c-a")))
    }

    @Test
    fun theShellShowsBootstrapThenHomeThenRescue() {
        assertEquals(ShellView.Loading, shellView(ConstructState()))
        val signedOut = ConstructState(seq = 1, auth = AuthInfo(session = false))
        assertEquals(ShellView.Bootstrap, shellView(signedOut))
        val open = ConstructState(seq = 1, auth = AuthInfo(session = true))
        assertEquals(ShellView.Home, shellView(open.copy(workspace = UiNode.Spacer())))
        assertEquals(ShellView.Rescue, shellView(open.copy(error = "409")))
        assertEquals(ShellView.Loading, shellView(open))
        // The bundled layouts parse.
        // Plumbing (decision `plumbing-and-porcelain`): no pod expands them.
        assertIs<UiNode.Page>(BundledLayouts.bootstrap)
        assertIs<UiNode.Page>(BundledLayouts.rescue)
    }

    @Test
    fun deviceActionsPresentSettingsAndTheInboxAndRetiredOnesDoNothing() {
        val bridge = Bridge(HOME_APP)
        val controller = ConstructController(bridge, HOME_APP, Dispatchers.Unconfined)
        assertTrue(controller.shellMode)
        bridge.emit(homeJson)
        controller.dispatchWithForm(ActionIntent.Device("open_settings"), emptyMap())
        val settings = assertIs<LayoutEntry>(controller.navigation.top)
        assertEquals(SETTINGS_LAYOUT to PresentationMode.Panel, settings.layoutId to settings.presentation)
        controller.dispatchWithForm(ActionIntent.Device("open_inbox"), emptyMap())
        val inbox = assertIs<LayoutEntry>(controller.navigation.top)
        assertEquals(INBOX_LAYOUT to PresentationMode.Sheet, inbox.layoutId to inbox.presentation)
        // Retired and unknown commands do nothing and never reach the engine.
        controller.dispatchWithForm(ActionIntent.Device("open_app", "c-bbbbbbbbbb"), emptyMap())
        controller.dispatchWithForm(ActionIntent.Device("show", id = "settings"), emptyMap())
        controller.dispatchWithForm(ActionIntent.Device("no_such_command"), emptyMap())
        assertEquals(2, controller.navigation.stack.size)
        assertTrue(bridge.dispatched.isEmpty())
        // Back pops one at a time.
        assertTrue(controller.navigation.back())
        assertEquals(SETTINGS_LAYOUT, controller.navigation.currentLayout())
        // Leaving the pod closes everything and forgets the session.
        controller.dispatchWithForm(ActionIntent.Device("leave_pod"), emptyMap())
        assertNull(controller.navigation.top)
        assertEquals(1, bridge.forgotten)
    }

    @Test
    fun requestNotificationPermissionAsksThePlatformAndDoesNothingWithoutOne() {
        val bridge = Bridge(HOME_APP)
        val controller = ConstructController(bridge, HOME_APP, Dispatchers.Unconfined)
        bridge.emit(homeJson)
        // Desktop and the simulator: no permission, so nothing happens and nothing reaches the engine.
        assertEquals("", NoNotificationPermission.state.value)
        controller.dispatchWithForm(ActionIntent.Device("request_notification_permission"), emptyMap())
        assertNull(controller.navigation.top)
        assertTrue(bridge.dispatched.isEmpty())
        // A platform with one (Android) is asked, and its state is the device data's.
        val phone = object : NotificationPermissionPlatform {
            override val state = kotlinx.coroutines.flow.MutableStateFlow("denied")
            override fun request() { state.value = "granted" }
        }
        controller.notificationPermission = phone
        val before = deviceData(ConstructState(), UnreadCounts(), emptyList(), "android", notificationPermission = phone.state.value)
        assertEquals(JsonPrimitive("denied"), before.entities["local://device"]!!["notification_permission"])
        controller.dispatchWithForm(ActionIntent.Device("request_notification_permission"), emptyMap())
        assertEquals("granted", phone.state.value)
        assertTrue(bridge.dispatched.isEmpty())
    }

    @Test
    fun aSheetFromALayoutNodeClearsItsFieldWhenDismissed() {
        val bridge = Bridge(HOME_APP)
        val controller = ConstructController(bridge, HOME_APP, Dispatchers.Unconfined)
        val form = controller.navigation.rootForm
        form["inbox"] = "1"
        controller.navigation.presentFromNode("node-1", INBOX_LAYOUT, emptyMap(), PresentationMode.Sheet, null) {
            form.remove("inbox")
        }
        controller.navigation.presentFromNode("node-1", INBOX_LAYOUT, emptyMap(), PresentationMode.Sheet, null) {}
        assertEquals(1, controller.navigation.stack.size, "one entry per node")
        assertTrue(controller.navigation.back())
        assertNull(form["inbox"], "Back clears show_when's field, so it stays closed")
        // The node going away takes its entry (and what is above it) with it.
        controller.navigation.presentFromNode("node-2", INBOX_LAYOUT, emptyMap(), PresentationMode.Sheet, null) {}
        controller.navigation.present("c-x")
        controller.navigation.dismissNode("node-2")
        assertNull(controller.navigation.top)
    }

    @Test
    fun signInGoesToATypedServerOrTheDefault() = kotlinx.coroutines.runBlocking {
        val bridge = Bridge(HOME_APP)
        val controller = ConstructController(bridge, HOME_APP, Dispatchers.Unconfined)
        controller.device.loadSignIns()
        val signIn = ActionIntent.Device("sign_in", id = "pixelfed", value = "${'$'}form?.server")
        controller.dispatchWithForm(signIn, mapOf("server" to "photos.example"))
        controller.dispatchWithForm(signIn, emptyMap())
        val sent = bridge.dispatched.map { ConstructJson.decodeIntent(it) as ActionIntent.AuthLogin }
        assertEquals(listOf("photos.example", "https://default.example"), sent.map { it.instanceUrl })
        val provider = deviceData(ConstructState(), UnreadCounts(), emptyList(), "desktop",
            DeviceSources(providers = controller.device.providers)).entities["local://providers/pixelfed"]!!
        assertEquals(JsonPrimitive(true), provider["custom_server"])
        assertEquals(JsonPrimitive("pixelfed.social"), provider["server_placeholder"])
    }
}
