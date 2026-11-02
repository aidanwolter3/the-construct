package org.genui.canvas

import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue

class ModelTest {
    @Test
    fun decodesCounterBundle() {
        val spec = CanvasJson.decodeBundle(COUNTER_BUNDLE_JSON)
        assertEquals("1.0.0", spec.version)
        val ws = assertIs<UiNode.Workspace>(spec.workspace)
        assertEquals("home", ws.id)
        assertEquals("Home", ws.title)
        val slot = ws.slots.single()
        assertEquals("counter", slot.slotId)
        assertEquals("core://counter", slot.providerUri)

        val root = assertIs<UiNode.Container>(slot.content)
        assertEquals(Orientation.Vertical, root.orientation)
        assertEquals(24, root.paddingDp)
        assertEquals(TextStyle.Title, assertIs<UiNode.Text>(root.children[0]).style)
        val headline = assertIs<UiNode.Text>(root.children[1])
        assertEquals("Count: \${core://counters/home#count}", headline.valueExpr)

        val row = assertIs<UiNode.Container>(root.children[2])
        assertEquals(Orientation.Horizontal, row.orientation)
        val buttons = row.children.map { assertIs<UiNode.Button>(it) }
        assertEquals(listOf("Increment", "Reset"), buttons.map { it.label })
        val inc = assertIs<ActionIntent.OptimisticMutate>(buttons[0].onClick)
        assertEquals("core://counters/home", inc.entityUri)
        assertEquals("count", inc.field)
        assertEquals("entity.count + 1", inc.valueExpr)
        assertEquals(JsonPrimitive(0), inc.rollbackValue)
        assertEquals("/v1/counters/home/increment", inc.endpoint)
        assertEquals("0", assertIs<ActionIntent.OptimisticMutate>(buttons[1].onClick).valueExpr)

        assertEquals(JsonPrimitive(0), spec.entities.getValue("core://counters/home")["count"])
    }

    @Test
    fun decodesPixelfedBundle() {
        val spec = CanvasJson.decodeBundle(PIXELFED_BUNDLE_JSON)
        val ws = assertIs<UiNode.Workspace>(spec.workspace)
        val feed = assertIs<UiNode.FeedList>(ws.slots.single().content)
        assertEquals("timeline:home", feed.queryKey)

        val card = assertIs<UiNode.PostCard>(feed.itemTemplate)
        assertEquals("\$item.uri", card.postRef)
        val author = assertIs<UiNode.AuthorHeader>(card.authorSlot)
        assertEquals("\$item.author_uri", author.userRef)
        assertNull(author.onClick)
        val media = assertIs<UiNode.MediaCarousel>(card.mediaSlot)
        assertEquals(MediaRefs.Expr("\$item.media_uris"), media.mediaRefs)
        assertEquals(1.0f, media.aspectRatio)

        val bar = assertIs<UiNode.ActionBar>(card.actionSlot)
        val like = assertIs<ActionIntent.OptimisticMutate>(bar.likeIntent)
        assertEquals(JsonPrimitive(false), like.rollbackValue)
        assertEquals(buildJsonObject { put("id", "\$item.id") }, like.payload)
        assertEquals(
            ActionIntent.Navigate("pixelfed://bookmarks", PresentationMode.Push),
            bar.bookmarkIntent,
        )
        assertEquals(PresentationMode.Modal, assertIs<ActionIntent.Navigate>(bar.commentIntent).presentation)
        assertEquals(TextStyle.Body, assertIs<UiNode.Text>(card.captionSlot).style)

        assertEquals(3, spec.entities.size)
        assertEquals("Sunrise over Boulder", spec.entities.getValue("pixelfed://posts/101")["caption"]?.jsonPrimitive?.content)
    }

    @Test
    fun unknownNodeTypeInsideContainerDecodesToUnknown() {
        val json = """
            {"type":"core://Container","orientation":"Horizontal","padding_dp":4,"children":[
              {"type":"future://Hologram","beam":"blue"},
              {"type":"core://Text","value_expr":"hi","style":"Caption"}
            ]}
        """.trimIndent()
        val c = assertIs<UiNode.Container>(CanvasJson.decodeNode(json))
        val unknown = assertIs<UiNode.Unknown>(c.children[0])
        assertEquals("future://Hologram", unknown.type)
        assertEquals("blue", unknown.raw["beam"]?.jsonPrimitive?.content)
        assertIs<UiNode.Text>(c.children[1])
        // Unknown nodes re-encode as their original object.
        assertEquals(c, CanvasJson.decodeNode(CanvasJson.encodeNode(c)))
    }

    @Test
    fun unknownIntentDecodesToUnknown() {
        val i = CanvasJson.decodeIntent("""{"intent":"action.future.teleport","to":"mars"}""")
        val u = assertIs<ActionIntent.Unknown>(i)
        assertEquals("action.future.teleport", u.intent)
        assertEquals("mars", u.raw["to"]?.jsonPrimitive?.content)
    }

    @Test
    fun mediaRefsLiteralListRoundTrips() {
        val node = CanvasJson.decodeNode("""{"type":"pixelfed://MediaCarousel","media_refs":["a","b"],"aspect_ratio":1.5}""")
        val m = assertIs<UiNode.MediaCarousel>(node)
        assertEquals(MediaRefs.Items(listOf("a", "b")), m.mediaRefs)
        assertTrue(CanvasJson.encodeNode(m).contains("\"media_refs\":[\"a\",\"b\"]"))
    }

    @Test
    fun bundleRoundTripsWithTypeDiscriminator() {
        val spec = CanvasJson.decodeBundle(PIXELFED_BUNDLE_JSON)
        val encoded = CanvasJson.encodeBundle(spec)
        assertTrue(encoded.contains("\"type\":\"core://Workspace\""))
        assertTrue(encoded.contains("\"media_refs\":\"\$item.media_uris\""))
        assertTrue(!encoded.contains("\"on_click\""), "null on_click must be omitted (D3)")
        assertEquals(spec, CanvasJson.decodeBundle(encoded))
    }

    @Test
    fun encodeIntentRoundTripsWithDiscriminator() {
        val mutate = ActionIntent.OptimisticMutate(
            entityUri = "core://counters/home",
            field = "count",
            valueExpr = "entity.count + 1",
            rollbackValue = JsonPrimitive(0),
            endpoint = "/v1/counters/home/increment",
            payload = buildJsonObject { },
        )
        val json = CanvasJson.encodeIntent(mutate)
        assertTrue(json.startsWith("{\"intent\":\"action.mutation.optimistic\""), json)
        val obj = CanvasJson.json.parseToJsonElement(json).jsonObject
        assertEquals("core://counters/home", obj["entity_uri"]?.jsonPrimitive?.content)
        assertEquals("entity.count + 1", obj["value_expr"]?.jsonPrimitive?.content)
        assertEquals(mutate, CanvasJson.decodeIntent(json))

        assertEquals("""{"intent":"action.navigation.back"}""", CanvasJson.encodeIntent(ActionIntent.NavigateBack))
        assertEquals(ActionIntent.NavigateBack, CanvasJson.decodeIntent("""{"intent":"action.navigation.back"}"""))

        for (i in listOf(
            ActionIntent.Navigate("pixelfed://bookmarks", PresentationMode.Replace),
            ActionIntent.AuthLogin("https://pixelfed.social"),
            ActionIntent.PromptAgent("show me cats"),
        )) {
            assertEquals(i, CanvasJson.decodeIntent(CanvasJson.encodeIntent(i)))
        }
    }

    @Test
    fun promptAgentWireFormat() {
        assertEquals(
            """{"intent":"action.agent.prompt","prompt_text":"Hide captions"}""",
            CanvasJson.encodeIntent(ActionIntent.PromptAgent("Hide captions")),
        )
    }

    @Test
    fun decodesEntitiesMap() {
        val e = CanvasJson.decodeEntities("""{"core://counters/home":{"id":"home","count":3}}""")
        assertEquals(JsonPrimitive(3), e.getValue("core://counters/home")["count"])
    }

    @Test
    fun mutateAlsoRoundTripsAndIsOmittedWhenEmpty() {
        val d = '$'
        val wire = """{"intent":"action.mutation.optimistic","entity_uri":"pixelfed://posts/1","field":"is_liked",""" +
            """"value_expr":"!entity.is_liked","rollback_value":false,"endpoint":"/v1/pixelfed/posts/like",""" +
            """"payload":{"id":"1","liked":"${d}value"},""" +
            """"also":[{"field":"like_count","value_expr":"entity.is_liked ? entity.like_count - 1 : entity.like_count + 1"}]}"""
        val intent = assertIs<ActionIntent.OptimisticMutate>(CanvasJson.decodeIntent(wire))
        assertEquals(
            listOf(FieldUpdate("like_count", "entity.is_liked ? entity.like_count - 1 : entity.like_count + 1")),
            intent.also,
        )
        // Byte-for-byte re-encode (Kotlin re-encodes intents before dispatching to Rust).
        assertEquals(wire, CanvasJson.encodeIntent(intent))

        val noAlso = """{"intent":"action.mutation.optimistic","entity_uri":"e","field":"f","value_expr":"0",""" +
            """"rollback_value":0,"endpoint":"/x","payload":{}}"""
        val plain = assertIs<ActionIntent.OptimisticMutate>(CanvasJson.decodeIntent(noAlso))
        assertEquals(emptyList(), plain.also)
        assertEquals(noAlso, CanvasJson.encodeIntent(plain))
    }

    @Test
    fun bundleQueriesDecodeInOrderAndAreOmittedWhenEmpty() {
        val spec = CanvasJson.decodeBundle(COUNTER_BUNDLE_JSON)
        assertEquals(emptyMap(), spec.queries)
        assertTrue(!CanvasJson.encodeBundle(spec).contains("\"queries\""))

        val withQueries = CanvasJson.decodeBundle(
            """{"version":"1","workspace":{"type":"core://Text","value_expr":"x","style":"Body"},""" +
                """"queries":{"timeline:home":["p://3","p://1","p://2"]}}"""
        )
        assertEquals(listOf("p://3", "p://1", "p://2"), withQueries.queries["timeline:home"])
        assertEquals(withQueries, CanvasJson.decodeBundle(CanvasJson.encodeBundle(withQueries)))
    }
}
