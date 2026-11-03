package org.theconstruct

import kotlin.random.Random
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

private class RecordingBridge : ConstructEngineBridge {
    val sent = mutableListOf<String>()
    var listener: ((EngineSnapshot) -> Unit)? = null
    override fun subscribe(onState: (EngineSnapshot) -> Unit) {
        listener = onState
    }
    override fun dispatch(intentJson: String) {
        sent += intentJson
    }
}

private val WORKSPACE_WITH_SCREEN = """
{"type":"core://Workspace","id":"app","title":"App","slots":[
  {"slot_id":"s","provider_uri":"p","content":{"type":"core://Text","value_expr":"Home","style":"Body"}}],
 "screens":[{"id":"thread","route":"x://posts/{id}/thread","title":"Thread $'$'route.id",
   "data":"/v1/x/thread?id=$'$'route.id",
   "content":{"type":"core://List","query_key":"thread:$'$'route.id","empty_text":"Nothing yet",
     "indent":{"field":"depth","step_dp":24},
     "item_template":{"type":"core://Text","value_expr":"$'$'item.text","style":"Body"}},
   "footer":{"type":"core://TextField","field":"reply","placeholder":"Reply","max_length":10,
     "on_submit":{"intent":"action.mutation.create","query_key":"thread:$'$'route.id",
       "entity_uri":"x://items/pending-$'$'nonce","entity":{"text":"$'$'form.reply"},
       "endpoint":"/v1/x/reply","payload":{"id":"$'$'route.id","text":"$'$'form.reply","key":"$'$'nonce"}}}}]}
""".replace("$'$'", "$")

class ScreensTest {
    @Test
    fun routesMatchWholeSegments() {
        assertEquals(mapOf("id" to "42"), matchRoute("x://posts/{id}/thread", "x://posts/42/thread"))
        assertEquals(mapOf("a" to "1", "b" to "z_9-"), matchRoute("x://{a}/m/{b}", "x://1/m/z_9-"))
        for (uri in listOf(
            "y://posts/42/thread", "x://posts/42", "x://posts/42/thread/more", "x://posts//thread",
            "x://posts/4 2/thread", "x://posts/a.b/thread", "x://posts/42/other", "nonsense",
        )) {
            assertNull(matchRoute("x://posts/{id}/thread", uri), uri)
        }
    }

    @Test
    fun newNodesAndIntentsRoundTripInTheRustShape() {
        val ws = assertIs<UiNode.Workspace>(ConstructJson.decodeNode(WORKSPACE_WITH_SCREEN))
        val screen = ws.screens.single()
        val list = assertIs<UiNode.ItemList>(screen.content)
        assertEquals(Indent("depth", 24), list.indent)
        val field = assertIs<UiNode.TextField>(screen.footer)
        assertEquals(10, field.maxLength)
        val create = assertIs<ActionIntent.Create>(field.onSubmit)
        assertEquals(false, create.refreshWorkspace)
        val again = ConstructJson.decodeNode(ConstructJson.encodeNode(ws))
        assertEquals(ws, again)
        // Defaults are omitted, like serde's skip_serializing_if.
        val encoded = ConstructJson.encodeIntent(create)
        assertTrue("refresh_workspace" !in encoded, encoded)
        assertTrue(encoded.startsWith("""{"intent":"action.mutation.create""""), encoded)
        assertEquals(
            """{"intent":"action.data.load","path":"/v1/x"}""",
            ConstructJson.encodeIntent(ActionIntent.LoadData("/v1/x")),
        )
        val image = ConstructJson.decodeNode("""{"type":"core://Image","url":"${'$'}item.a"}""")
        assertEquals(UiNode.Image(url = "\$item.a"), image)
    }

    @Test
    fun routeBindingReplacesEveryRouteToken() {
        val ws = assertIs<UiNode.Workspace>(ConstructJson.decodeNode(WORKSPACE_WITH_SCREEN))
        val bound = bindRoute(ws.screens.single(), mapOf("id" to "42"))
        assertEquals("Thread 42", bound.title)
        assertEquals("/v1/x/thread?id=42", bound.data)
        assertEquals("thread:42", assertIs<UiNode.ItemList>(bound.content).queryKey)
        val create = assertIs<ActionIntent.Create>(assertIs<UiNode.TextField>(bound.footer).onSubmit)
        assertEquals("42", create.payload.jsonObject["id"]!!.jsonPrimitive.content)
        // Form and nonce tokens are left for dispatch time; the route pattern itself is kept.
        assertEquals("\$form.reply", create.payload.jsonObject["text"]!!.jsonPrimitive.content)
        assertEquals("x://posts/{id}/thread", bound.route)
    }

    @Test
    fun formBindingFillsValuesAndOneNonceAndSkipsBlankFields() {
        val intent = ActionIntent.Create(
            queryKey = "q", entityUri = "x://i/pending-\$nonce",
            entity = JsonObject(mapOf("text" to JsonPrimitive("\$form.reply"))),
            endpoint = "/v1/x", payload = JsonObject(mapOf("t" to JsonPrimitive("\$form.reply"), "k" to JsonPrimitive("\$nonce"))),
        )
        val bound = assertIs<ActionIntent.Create>(bindForm(intent, mapOf("reply" to "  hi \$nonce ")) { "N1" })
        assertEquals("x://i/pending-N1", bound.entityUri)
        assertEquals("N1", bound.payload.jsonObject["k"]!!.jsonPrimitive.content)
        // Typed text is not re-scanned for tokens.
        assertEquals("hi \$nonce", bound.payload.jsonObject["t"]!!.jsonPrimitive.content)
        assertNull(bindForm(intent, mapOf("reply" to "   ")))
        assertNull(bindForm(intent, emptyMap()))
        // value_expr is the engine's; intents without tokens are returned as they are.
        val mutate = ActionIntent.OptimisticMutate(
            "x://a", "n", "entity.n + 1", JsonPrimitive(0), "/v1/x", JsonObject(emptyMap()),
        )
        assertEquals(mutate, bindForm(mutate, emptyMap()))
        val nonce = newNonce(Random(3))
        assertEquals(24, nonce.length)
        assertTrue(nonce.all { it in '0'..'9' || it in 'a'..'f' })
    }

    @Test
    fun navigationOpensMatchingScreensAndLoadsTheirData() {
        val bridge = RecordingBridge()
        val controller = ConstructController(bridge)
        bridge.listener!!(EngineSnapshot(WORKSPACE_WITH_SCREEN, "{}", true, 1, 0))
        controller.dispatch(ActionIntent.Navigate("x://posts/42/thread", PresentationMode.Push))
        val top = controller.navigation.top!!
        assertEquals("thread", top.screen.id)
        assertEquals(mapOf("id" to "42"), top.params)
        assertEquals(listOf("""{"intent":"action.data.load","path":"/v1/x/thread?id=42"}"""), bridge.sent)
        // Replace swaps the top screen; Push stacks.
        controller.dispatch(ActionIntent.Navigate("x://posts/43/thread", PresentationMode.Replace))
        assertEquals(1, controller.navigation.stack.size)
        assertEquals("43", controller.navigation.top!!.params["id"])
        controller.dispatch(ActionIntent.Navigate("x://posts/44/thread", PresentationMode.Push))
        assertEquals(2, controller.navigation.stack.size)
        // A target no screen matches goes to the engine.
        controller.dispatch(ActionIntent.Navigate("x://elsewhere", PresentationMode.Push))
        assertTrue(bridge.sent.last().contains("x://elsewhere"))
        // Back pops; with no screen open it goes to the engine.
        controller.dispatch(ActionIntent.NavigateBack)
        controller.dispatch(ActionIntent.NavigateBack)
        assertNull(controller.navigation.top)
        val before = bridge.sent.size
        controller.dispatch(ActionIntent.NavigateBack)
        assertEquals(before + 1, bridge.sent.size)
    }

    @Test
    fun submittingBindsTheVisibleScreensForm() {
        val bridge = RecordingBridge()
        val controller = ConstructController(bridge)
        bridge.listener!!(EngineSnapshot(WORKSPACE_WITH_SCREEN, "{}", true, 1, 0))
        controller.dispatch(ActionIntent.Navigate("x://posts/42/thread", PresentationMode.Push))
        val entry = controller.navigation.top!!
        val submit = assertIs<UiNode.TextField>(entry.screen.footer).onSubmit!!
        controller.dispatch(submit) // blank: nothing sent
        assertEquals(1, bridge.sent.size)
        entry.form["reply"] = "Nice"
        controller.dispatch(submit)
        val sent = assertIs<ActionIntent.Create>(ConstructJson.decodeIntent(bridge.sent.last()))
        assertEquals("thread:42", sent.queryKey)
        assertEquals("Nice", sent.entity.jsonObject["text"]!!.jsonPrimitive.content)
        val key = sent.payload.jsonObject["key"]!!.jsonPrimitive.content
        assertEquals("x://items/pending-$key", sent.entityUri)
    }

    @Test
    fun loadStatesListItemsAndIndents() {
        assertEquals(
            mapOf("/a" to LoadState("ready"), "/b" to LoadState("error", "Nope")),
            LoadState.parseAll("""{"/a":{"state":"ready"},"/b":{"state":"error","error":"Nope"},"/c":3}"""),
        )
        assertEquals(emptyMap(), LoadState.parseAll("not json"))
        val entities = mapOf(
            "x://1" to JsonObject(mapOf("depth" to JsonPrimitive(2))),
            "x://2" to JsonObject(mapOf("depth" to JsonPrimitive(9))),
        )
        assertNull(listItems("q", emptyMap(), entities), "absent query: not loaded yet")
        val items = listItems("q", mapOf("q" to listOf("x://2", "x://missing", "x://1", "x://2")), entities)!!
        assertEquals(listOf("x://2", "x://1"), items.map { it["uri"]!!.jsonPrimitive.content })
        assertEquals(96, indentDp(Indent("depth", 24), items[0]), "capped at 4 steps")
        assertEquals(48, indentDp(Indent("depth", 24, max = 8), items[1]))
        assertEquals(0, indentDp(null, items[1]))
        assertEquals(0, indentDp(Indent("nope", 24), items[1]))
    }

    @Test
    fun listFilterMatchesEveryWordIgnoringCaseAndAccents() {
        val item = JsonObject(
            mapOf(
                "title" to JsonPrimitive("Crème Brûlée"),
                "search_text" to JsonPrimitive("Crème Brûlée · Desserts · heavy cream · SUGAR"),
                "tags" to kotlinx.serialization.json.JsonArray(listOf(JsonPrimitive("french"), JsonPrimitive(3))),
                "n" to JsonPrimitive(42),
            )
        )
        assertEquals("creme brulee", foldForSearch("Crème Brûlée"))
        assertEquals("creme", foldForSearch("cre\u0300me"), "combining marks are dropped")
        assertTrue(matchesFilter("", item, listOf("title")), "blank: everything")
        assertTrue(matchesFilter("   ", item, listOf("title")))
        assertTrue(matchesFilter("creme", item, listOf("title")))
        assertTrue(matchesFilter("BRÛLÉE", item, listOf("title")))
        assertTrue(matchesFilter("  sugar   creme ", item, listOf("search_text")), "every word, any order")
        assertTrue(!matchesFilter("sugar rhubarb", item, listOf("search_text")), "every word must match")
        assertTrue(!matchesFilter("sugar", item, listOf("title")), "only the listed fields")
        assertTrue(matchesFilter("sugar french", item, listOf("title", "search_text", "tags")), "words may match different fields")
        assertTrue(matchesFilter("42", item, listOf("n")), "numbers are text")
        assertTrue(!matchesFilter("x", item, listOf("missing")))

        val f = ListFilter("\$form.q", listOf("title"))
        assertEquals("", filterQuery(f, null))
        assertEquals("", filterQuery(f, mapOf("other" to "x")))
        assertEquals("pie", filterQuery(f, mapOf("q" to "pie")))
        assertEquals("pie", filterQuery(ListFilter("pie", listOf("title")), null), "a literal value")
    }

    @Test
    fun groupHeadersFollowTheDrawnItems() {
        fun item(cat: String?) = JsonObject(if (cat == null) emptyMap() else mapOf("category" to JsonPrimitive(cat)))
        val items = listOf(item("A"), item("A"), item("B"), item(null), item(null), item("A"))
        assertEquals(listOf(true, false, true, true, false, true), groupStarts(items, "category"))
        // Filtering out a group's first item moves its header to the next one drawn.
        assertEquals(listOf(true, true), groupStarts(listOf(items[1], items[2]), "category"))
        assertEquals(emptyList(), groupStarts(emptyList(), "category"))
    }
}
