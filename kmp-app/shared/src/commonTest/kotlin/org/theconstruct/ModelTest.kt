package org.theconstruct

import kotlinx.serialization.json.JsonObject
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
        val spec = ConstructJson.decodeBundle(COUNTER_BUNDLE_JSON)
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
        val spec = ConstructJson.decodeBundle(PIXELFED_BUNDLE_JSON)
        val ws = assertIs<UiNode.Workspace>(spec.workspace)
        val feed = assertIs<UiNode.ItemList>(ws.slots.single().content)
        assertEquals("timeline:home", feed.queryKey)
        assertEquals("No posts", feed.emptyText)

        val card = assertIs<UiNode.Column>(feed.itemTemplate)
        val boosted = assertIs<UiNode.Conditional>(card.children[0])
        assertEquals("\$item.reblogged_by_name", boosted.value)
        assertEquals("repeat", assertIs<UiNode.Badge>(boosted.content).icon)
        val header = assertIs<UiNode.Row>(card.children[1])
        val avatar = assertIs<UiNode.Image>(header.children[0])
        assertEquals(ImageShape.Circle, avatar.shape)
        val media = assertIs<UiNode.Carousel>(card.children[2])
        assertEquals(ListBinding.Expr("\$item.media_uris"), media.items)
        assertEquals("aspect_ratio", media.aspectField)
        assertEquals(0.8, media.minAspect)
        assertEquals(1.91, media.maxAspect)
        assertIs<UiNode.Image>(media.itemTemplate)

        val bar = assertIs<UiNode.Row>(card.children[3])
        val like = assertIs<UiNode.IconButton>(bar.children[0])
        assertEquals("favorite_border", like.icon)
        assertEquals("favorite", like.activeIcon)
        assertEquals("\$item.is_liked", like.active)
        assertEquals("#ED4956", like.activeTint)
        val likeIntent = assertIs<ActionIntent.OptimisticMutate>(like.onClick)
        assertEquals(JsonPrimitive(false), likeIntent.rollbackValue)
        val comment = bar.children.filterIsInstance<UiNode.IconButton>().single { it.icon == "chat_bubble" }
        assertEquals(PresentationMode.Modal, assertIs<ActionIntent.Navigate>(comment.onClick).presentation)
        assertIs<UiNode.Spacer>(bar.children.first { it is UiNode.Spacer && it.sizeDp == null })
        assertEquals(UiNode.Divider, card.children.last())

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
        val c = assertIs<UiNode.Container>(ConstructJson.decodeNode(json))
        val unknown = assertIs<UiNode.Unknown>(c.children[0])
        assertEquals("future://Hologram", unknown.type)
        assertEquals("blue", unknown.raw["beam"]?.jsonPrimitive?.content)
        assertIs<UiNode.Text>(c.children[1])
        // Unknown nodes re-encode as their original object.
        assertEquals(c, ConstructJson.decodeNode(ConstructJson.encodeNode(c)))
    }

    @Test
    fun unknownIntentDecodesToUnknown() {
        val i = ConstructJson.decodeIntent("""{"intent":"action.future.teleport","to":"mars"}""")
        val u = assertIs<ActionIntent.Unknown>(i)
        assertEquals("action.future.teleport", u.intent)
        assertEquals("mars", u.raw["to"]?.jsonPrimitive?.content)
    }

    @Test
    fun carouselLiteralItemsRoundTrip() {
        val node = ConstructJson.decodeNode(
            """{"type":"core://Carousel","items":["a","b"],"item_template":{"type":"core://Text","value_expr":"x","style":"Body"},"aspect_ratio":1.5}"""
        )
        val m = assertIs<UiNode.Carousel>(node)
        assertEquals(ListBinding.Items(listOf("a", "b")), m.items)
        assertEquals(1.5, m.aspectRatio)
        assertTrue(ConstructJson.encodeNode(m).contains("\"items\":[\"a\",\"b\"]"))
        assertEquals(m, ConstructJson.decodeNode(ConstructJson.encodeNode(m)))
    }

    @Test
    fun removedPixelfedNodesDegradeToUnknown() {
        val raw = """{"type":"pixelfed://PostCard","post_ref":"x"}"""
        val u = assertIs<UiNode.Unknown>(ConstructJson.decodeNode(raw))
        assertEquals("pixelfed://PostCard", u.type)
    }

    @Test
    fun genericNodesRoundTrip() {
        val nodes = listOf(
            UiNode.Row(listOf(UiNode.Spacer(), UiNode.Divider), spacingDp = 4, paddingHDp = 2),
            UiNode.Column(listOf(UiNode.Spacer(8)), paddingVDp = 3),
            UiNode.Conditional("\$item.x", UiNode.Chip("Hi", "warning", Tone.Warning)),
            UiNode.Badge("b", "repeat"),
            UiNode.IconButton("send", "Send", ActionIntent.NavigateBack, active = "\$item.on", activeTint = "#112233", tag = "t"),
            UiNode.Image("u", blurhash = "LEHV6nWB2yk8"),
            UiNode.Tappable(
                ActionIntent.Navigate("recipes://recipes/\$item.id", PresentationMode.Push),
                UiNode.Row(listOf(UiNode.Text("\$item.title", TextStyle.Label))),
                contentDescription = "Open \$item.title",
                tag = "row:\$item.id",
            ),
            UiNode.Tappable(ActionIntent.NavigateBack, UiNode.Divider),
            UiNode.ItemList(
                "q", UiNode.Divider,
                filter = ListFilter("\$form.q", listOf("search_text"), noMatchText = "None"),
                groupBy = GroupBy("category", UiNode.Text("\$item.category", TextStyle.Title)),
            ),
            UiNode.TextField("q", placeholder = "Search"),
        )
        for (n in nodes) assertEquals(n, ConstructJson.decodeNode(ConstructJson.encodeNode(n)), n.toString())
        assertTrue(ConstructJson.encodeNode(UiNode.Divider) == "{\"type\":\"core://Divider\"}", ConstructJson.encodeNode(UiNode.Divider))
    }

    @Test
    fun schema6NodesDecodeFromTheWire() {
        val box = assertIs<UiNode.Box>(ConstructJson.decodeNode(
            """{"type":"core://Box","background":"${'$'}item.color","content_color":"#A3361B","corner_dp":12,
               "width_dp":44,"height_dp":44,"align":"Center","content":{"type":"core://Text","value_expr":"Q","style":"Display"}}"""
        ))
        assertEquals("\$item.color", box.background)
        assertEquals(Align.Center, box.align)
        assertEquals(TextStyle.Display, assertIs<UiNode.Text>(box.content).style)
        val chips = assertIs<UiNode.ChoiceChips>(ConstructJson.decodeNode(
            """{"type":"core://ChoiceChips","field":"cat","options":[{"label":"All"},{"label":"Mains","value":"Main Dishes"}]}"""
        ))
        assertEquals(listOf(ChoiceOption("All", ""), ChoiceOption("Mains", "Main Dishes")), chips.options)
        val list = assertIs<UiNode.ItemList>(ConstructJson.decodeNode(
            """{"type":"core://List","query_key":"q","item_template":{"type":"core://Divider"},
               "where":{"field":"category_short","equals":"${'$'}form.cat"}}"""
        ))
        assertEquals(ListWhere("category_short", "\$form.cat"), list.where)
        val row = assertIs<UiNode.Row>(ConstructJson.decodeNode("""{"type":"core://Row","children":[],"align":"Start"}"""))
        assertEquals(Align.Start, row.align)
        assertNull(assertIs<UiNode.Row>(ConstructJson.decodeNode("""{"type":"core://Row","children":[]}""")).align)
        for (n in listOf(box, chips, list, row, UiNode.Column(emptyList(), align = Align.Center))) {
            assertEquals(n, ConstructJson.decodeNode(ConstructJson.encodeNode(n)), n.toString())
        }
        assertTrue(!ConstructJson.encodeNode(UiNode.Column(emptyList())).contains("align"), "default align is omitted")
    }

    @Test
    fun listWhereAndGroupCounts() {
        val item = JsonObject(mapOf("category_short" to JsonPrimitive("Desserts")))
        assertTrue(matchesWhere("", item, "category_short"))
        assertTrue(matchesWhere("Desserts", item, "category_short"))
        assertTrue(!matchesWhere("Dessert", item, "category_short"), "exact match only")
        assertEquals("Mains", formOrLiteral("\$form.cat", mapOf("cat" to "Mains")))
        assertEquals("", formOrLiteral("\$form.cat", emptyMap()))
        assertEquals(listOf(2, 2, 1, 3, 3, 3), groupCounts(listOf(true, false, true, true, false, false)))
        assertEquals(emptyList(), groupCounts(emptyList()))
    }

    @Test
    fun listFilterAndGroupByDecodeFromTheWire() {
        val raw = """{"type":"core://List","query_key":"recipes:all","item_template":{"type":"core://Divider"},
            "filter":{"value":"${'$'}form.q","fields":["search_text"]},
            "group_by":{"field":"category","header":{"type":"core://Text","value_expr":"${'$'}item.category","style":"Title"}}}"""
        val l = assertIs<UiNode.ItemList>(ConstructJson.decodeNode(raw))
        assertEquals(ListFilter("\$form.q", listOf("search_text")), l.filter)
        assertEquals("category", l.groupBy?.field)
        assertIs<UiNode.Text>(l.groupBy?.header)
        val plain = assertIs<UiNode.ItemList>(ConstructJson.decodeNode("""{"type":"core://List","query_key":"q","item_template":{"type":"core://Divider"}}"""))
        assertNull(plain.filter)
        assertTrue(!ConstructJson.encodeNode(plain).contains("filter"), "null optional fields are omitted")
    }

    @Test
    fun tappableDecodesFromTheWireAndItsIntentBindsItem() {
        val raw = """{"type":"core://Tappable","on_click":{"intent":"action.navigation.open",
            "target_uri":"recipes://recipes/${'$'}item.id","presentation":"Push"},
            "content":{"type":"core://Text","value_expr":"${'$'}item.title","style":"Label"}}"""
        val t = assertIs<UiNode.Tappable>(ConstructJson.decodeNode(raw))
        assertIs<UiNode.Text>(t.content)
        assertEquals(null, t.contentDescription)
        val item = buildJsonObject { put("id", "7") }
        val bound = assertIs<ActionIntent.Navigate>(bindIntent(t.onClick, item))
        assertEquals("recipes://recipes/7", bound.targetUri)
        assertTrue(!ConstructJson.encodeNode(t).contains("content_description"), "null optional fields are omitted")
    }

    @Test
    fun bundleRoundTripsWithTypeDiscriminator() {
        val spec = ConstructJson.decodeBundle(PIXELFED_BUNDLE_JSON)
        val encoded = ConstructJson.encodeBundle(spec)
        assertTrue(encoded.contains("\"type\":\"core://Workspace\""))
        assertTrue(encoded.contains("\"items\":\"\$item.media_uris\""))
        assertTrue(!encoded.contains(":null"), "null optional fields must be omitted (D3)")
        assertEquals(spec, ConstructJson.decodeBundle(encoded))
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
        val json = ConstructJson.encodeIntent(mutate)
        assertTrue(json.startsWith("{\"intent\":\"action.mutation.optimistic\""), json)
        val obj = ConstructJson.json.parseToJsonElement(json).jsonObject
        assertEquals("core://counters/home", obj["entity_uri"]?.jsonPrimitive?.content)
        assertEquals("entity.count + 1", obj["value_expr"]?.jsonPrimitive?.content)
        assertEquals(mutate, ConstructJson.decodeIntent(json))

        assertEquals("""{"intent":"action.navigation.back"}""", ConstructJson.encodeIntent(ActionIntent.NavigateBack))
        assertEquals(ActionIntent.NavigateBack, ConstructJson.decodeIntent("""{"intent":"action.navigation.back"}"""))

        for (i in listOf(
            ActionIntent.Navigate("pixelfed://bookmarks", PresentationMode.Replace),
            ActionIntent.AuthLogin("https://pixelfed.social"),
            ActionIntent.AuthLogin("https://pixelfed.social", "/v1/auth/x/app", "/v1/auth/x/token"),
        )) {
            assertEquals(i, ConstructJson.decodeIntent(ConstructJson.encodeIntent(i)))
        }
    }

    @Test
    fun removedAgentIntentDegradesToUnknown() {
        val json = """{"intent":"action.agent.prompt","prompt_text":"Hide captions"}"""
        val i = ConstructJson.decodeIntent(json)
        assertEquals("action.agent.prompt", (i as ActionIntent.Unknown).intent)
        assertEquals(json, ConstructJson.encodeIntent(i))
    }

    @Test
    fun decodesEntitiesMap() {
        val e = ConstructJson.decodeEntities("""{"core://counters/home":{"id":"home","count":3}}""")
        assertEquals(JsonPrimitive(3), e.getValue("core://counters/home")["count"])
    }

    @Test
    fun mutateAlsoRoundTripsAndIsOmittedWhenEmpty() {
        val d = '$'
        val wire = """{"intent":"action.mutation.optimistic","entity_uri":"pixelfed://posts/1","field":"is_liked",""" +
            """"value_expr":"!entity.is_liked","rollback_value":false,"endpoint":"/v1/pixelfed/posts/like",""" +
            """"payload":{"id":"1","liked":"${d}value"},""" +
            """"also":[{"field":"like_count","value_expr":"entity.is_liked ? entity.like_count - 1 : entity.like_count + 1"}]}"""
        val intent = assertIs<ActionIntent.OptimisticMutate>(ConstructJson.decodeIntent(wire))
        assertEquals(
            listOf(FieldUpdate("like_count", "entity.is_liked ? entity.like_count - 1 : entity.like_count + 1")),
            intent.also,
        )
        // Byte-for-byte re-encode (Kotlin re-encodes intents before dispatching to Rust).
        assertEquals(wire, ConstructJson.encodeIntent(intent))

        val noAlso = """{"intent":"action.mutation.optimistic","entity_uri":"e","field":"f","value_expr":"0",""" +
            """"rollback_value":0,"endpoint":"/x","payload":{}}"""
        val plain = assertIs<ActionIntent.OptimisticMutate>(ConstructJson.decodeIntent(noAlso))
        assertEquals(emptyList(), plain.also)
        assertEquals(noAlso, ConstructJson.encodeIntent(plain))
    }

    @Test
    fun bundleQueriesDecodeInOrderAndAreOmittedWhenEmpty() {
        val spec = ConstructJson.decodeBundle(COUNTER_BUNDLE_JSON)
        assertEquals(emptyMap(), spec.queries)
        assertTrue(!ConstructJson.encodeBundle(spec).contains("\"queries\""))

        val withQueries = ConstructJson.decodeBundle(
            """{"version":"1","workspace":{"type":"core://Text","value_expr":"x","style":"Body"},""" +
                """"queries":{"timeline:home":["p://3","p://1","p://2"]}}"""
        )
        assertEquals(listOf("p://3", "p://1", "p://2"), withQueries.queries["timeline:home"])
        assertEquals(withQueries, ConstructJson.decodeBundle(ConstructJson.encodeBundle(withQueries)))
    }
}
