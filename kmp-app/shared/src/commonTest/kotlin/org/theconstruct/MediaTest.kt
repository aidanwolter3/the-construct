package org.theconstruct

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

private class Recorder : ConstructEngineBridge {
    val sent = mutableListOf<String>()
    var listener: ((EngineSnapshot) -> Unit)? = null
    override fun subscribe(onState: (EngineSnapshot) -> Unit) {
        listener = onState
    }
    override fun dispatch(intentJson: String) {
        sent += intentJson
    }
}

/** The compose screen of layouts/pixelfed_home/0009.json, reached from the feed: Share is [create, back]. */
private val COMPOSE_WORKSPACE = """
{"type":"core://Workspace","id":"pixelfed_home","title":"Home","slots":[
  {"slot_id":"s","provider_uri":"p","content":{"type":"core://IconButton","icon":"add_photo","content_description":"New post",
    "tag":"new-post","on_click":{"intent":"action.navigation.open","target_uri":"pixelfed://compose","presentation":"Modal"}}}],
 "screens":[{"id":"compose","route":"pixelfed://compose","title":"New post",
   "content":{"type":"core://Column","spacing_dp":12,"padding_h_dp":16,"padding_v_dp":12,"children":[
     {"type":"core://MediaPicker","field":"photos","max_items":4,"label":"Add photos"},
     {"type":"core://TextField","field":"caption","placeholder":"Write a caption…","max_length":2000,"lines":4}]},
   "footer":{"type":"core://Button","label":"Share","on_click":{"intent":"action.sequence","steps":[{"intent":"action.mutation.create","query_key":"timeline:home",
     "position":"start","entity_uri":"pixelfed://posts/pending-$'$'nonce",
     "entity":{"author_name":"You","caption":"$'$'form?.caption","media_uris":"$'$'attachments.photos","pending_label":"Posting…"},
     "attachments":{"photos":"$'$'form.photos"},"endpoint":"/v1/pixelfed/posts/create",
     "payload":{"caption":"$'$'form?.caption","media":"$'$'attachments.photos","client_id":"$'$'nonce"},
     "refresh_workspace":true},{"intent":"action.navigation.back"}]}}}]}
""".replace("$'$'", "$")

class MediaTest {
    private fun composeScreen(): Screen =
        assertIs<UiNode.Workspace>(ConstructJson.decodeNode(COMPOSE_WORKSPACE)).screens.single()

    private fun share(): ActionIntent.Create {
        val onClick = assertIs<ActionIntent.Sequence>(assertIs<UiNode.Button>(composeScreen().footer).onClick)
        assertEquals(ActionIntent.NavigateBack, onClick.steps[1])
        return assertIs<ActionIntent.Create>(onClick.steps[0])
    }

    @Test
    fun schema8NodesAndCreateFieldsParseAndRoundTrip() {
        val column = assertIs<UiNode.Column>(composeScreen().content)
        assertEquals(UiNode.MediaPicker("photos", 4, "Add photos"), column.children[0])
        val caption = assertIs<UiNode.TextField>(column.children[1])
        assertEquals(4, caption.lines)
        assertNull(caption.onSubmit)
        val create = share()
        assertEquals(CreatePosition.Start, create.position)
        assertEquals(mapOf("photos" to JsonPrimitive("\$form.photos")), create.attachments)
        val ws = ConstructJson.decodeNode(COMPOSE_WORKSPACE)
        assertEquals(ws, ConstructJson.decodeNode(ConstructJson.encodeNode(ws)))
        val encoded = ConstructJson.encodeIntent(create)
        assertTrue(""""position":"start"""" in encoded, encoded)
        // Older JSON still parses with the defaults, which are omitted again (serde's skip_serializing_if).
        val old = assertIs<ActionIntent.Create>(
            ConstructJson.decodeIntent(
                """{"intent":"action.mutation.create","query_key":"q","entity_uri":"x://a","entity":{},"endpoint":"/e","payload":{}}""",
            ),
        )
        assertEquals(CreatePosition.End, old.position)
        assertEquals(emptyMap(), old.attachments)
        val again = ConstructJson.encodeIntent(old)
        for (k in listOf("position", "attachments")) assertFalse(k in again, again)
        // Schema 13 removed `dismiss`: the pod never serves a version with it to this client, and it parses as nothing.
        assertEquals(
            old,
            ConstructJson.decodeIntent(
                """{"intent":"action.mutation.create","query_key":"q","entity_uri":"x://a","entity":{},"endpoint":"/e","payload":{},"dismiss":true}""",
            ),
        )
        assertNull(assertIs<UiNode.TextField>(ConstructJson.decodeNode("""{"type":"core://TextField","field":"f"}""")).lines)
        assertEquals(UiNode.MediaPicker("p", 2), ConstructJson.decodeNode("""{"type":"core://MediaPicker","field":"p","max_items":2}"""))
        for (icon in listOf("add_photo", "add", "refresh")) assertTrue(ConstructIcons.named(icon) != null, icon)
        // Feature `todo_icons`.
        for (icon in listOf("check", "check_box", "check_box_outline_blank", "delete")) {
            assertTrue(ConstructIcons.named(icon) != null, icon)
        }
    }

    @Test
    fun formListsRoundTrip() {
        assertEquals(listOf("a", "b"), parseFormList("""["a","b"]"""))
        assertEquals(emptyList(), parseFormList("[]"))
        assertEquals(listOf("a"), parseFormList(encodeFormList(listOf("a"))))
        for (v in listOf(null, "", "hello", "[1,2]", "[a]x", "[\"a\"")) assertNull(parseFormList(v), v)
    }

    @Test
    fun aListFieldBindsAsAnArrayAndBlocksWhenEmpty() {
        val create = share()
        val form = mapOf("photos" to encodeFormList(listOf("id1", "id2")), "caption" to " Sunset ")
        val bound = assertIs<ActionIntent.Create>(bindForm(create, form) { "N" })
        assertEquals(JsonArray(listOf(JsonPrimitive("id1"), JsonPrimitive("id2"))), bound.attachments["photos"])
        assertEquals("pixelfed://posts/pending-N", bound.entityUri)
        assertEquals("Sunset", bound.entity.jsonObject["caption"]!!.jsonPrimitive.content)
        assertEquals("Sunset", bound.payload.jsonObject["caption"]!!.jsonPrimitive.content)
        // `$attachments.` tokens are the engine's.
        assertEquals("\$attachments.photos", bound.entity.jsonObject["media_uris"]!!.jsonPrimitive.content)
        assertEquals("\$attachments.photos", bound.payload.jsonObject["media"]!!.jsonPrimitive.content)
        // No photos (unset or an emptied list): nothing is sent.
        assertNull(bindForm(create, mapOf("caption" to "x")))
        assertNull(bindForm(create, mapOf("photos" to "[]", "caption" to "x")))
    }

    @Test
    fun anOptionalFieldBindsBlankAndNeverBlocks() {
        val bound = assertIs<ActionIntent.Create>(bindForm(share(), mapOf("photos" to encodeFormList(listOf("a")))) { "N" })
        assertEquals("", bound.entity.jsonObject["caption"]!!.jsonPrimitive.content)
        assertEquals("", bound.payload.jsonObject["caption"]!!.jsonPrimitive.content)
        assertEquals(1, bound.attachments["photos"]!!.jsonArray.size)
        // Inside a longer string too; a required field still blocks.
        val intent = ActionIntent.Create(
            "q", "x://a", JsonObject(mapOf("t" to JsonPrimitive("Re: \$form?.title (\$form.body)"))), "/e", JsonObject(emptyMap()),
        )
        assertEquals(
            "Re:  (hi)",
            assertIs<ActionIntent.Create>(bindForm(intent, mapOf("body" to "hi"))).entity.jsonObject["t"]!!.jsonPrimitive.content,
        )
        assertNull(bindForm(intent, mapOf("title" to "T")))
        // An optional list binds to its (possibly empty) array.
        val opt = ActionIntent.Create(
            "q", "x://a", JsonObject(emptyMap()), "/e", JsonObject(emptyMap()), attachments = mapOf("p" to JsonPrimitive("\$form?.p")),
        )
        assertEquals(JsonArray(emptyList()), assertIs<ActionIntent.Create>(bindForm(opt, mapOf("p" to "[]"))).attachments["p"])
    }

    @Test
    fun aSequenceOfCreateAndBackClosesTheComposeScreen() {
        assertEquals(CreatePosition.Start, shareFromCompose(COMPOSE_WORKSPACE).position)
    }

    /** Opens the compose screen of [workspace], taps Share without and then with a photo; returns what was sent. */
    private fun shareFromCompose(workspace: String): ActionIntent.Create {
        val bridge = Recorder()
        val controller = ConstructController(bridge)
        bridge.listener!!(EngineSnapshot(workspace, "{}", true, 1, 0))
        controller.dispatch(ActionIntent.Navigate("pixelfed://compose", PresentationMode.Modal))
        val entry = controller.navigation.top!!
        assertEquals("compose", entry.screen.id)
        val share = assertIs<UiNode.Button>(entry.screen.footer).onClick
        controller.dispatch(share) // no photos: nothing sent, the screen stays
        assertTrue(bridge.sent.isEmpty(), bridge.sent.toString())
        assertEquals(entry, controller.navigation.top)
        entry.form["photos"] = encodeFormList(listOf("att1"))
        entry.form["caption"] = "Hello"
        controller.dispatch(share)
        // Only the create reaches the engine (the back is the client's), and the screen is closed.
        val sent = assertIs<ActionIntent.Create>(ConstructJson.decodeIntent(bridge.sent.single()))
        assertEquals(JsonArray(listOf(JsonPrimitive("att1"))), sent.attachments["photos"])
        assertEquals(CreatePosition.Start, sent.position)
        assertNull(controller.navigation.top)
        return sent
    }
}
