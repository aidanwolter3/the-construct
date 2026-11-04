package org.theconstruct

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNull

class TextFormatTest {
    private val today = isoDateEpochDay("2026-11-04")!!

    private fun item(vararg pairs: Pair<String, Any>) = JsonObject(
        pairs.associate { (k, v) -> k to if (v is Boolean) JsonPrimitive(v) else JsonPrimitive(v.toString()) }
    )

    private fun text(json: String) = assertIs<UiNode.Text>(ConstructJson.decodeNode(json))

    private fun draw(
        node: UiNode.Text,
        item: JsonObject? = null,
        queries: Map<String, List<String>> = emptyMap(),
        entities: Map<String, JsonObject> = emptyMap(),
        form: Map<String, String>? = null,
    ) = formatText(node, entities, item, queries, form, today)

    @Test
    fun parsesOnlyRealIsoDates() {
        assertEquals(0L, isoDateEpochDay("1970-01-01"))
        assertEquals(20_761L, isoDateEpochDay("2026-11-04"))
        assertEquals(isoDateEpochDay("2028-03-01")!! - 1, isoDateEpochDay("2028-02-29"))
        for (bad in listOf("2026-02-29", "2026-13-01", "2026-11-31", "2026-11-4", "Nov 4", "2026-11-04T10:00:00Z", "")) {
            assertNull(isoDateEpochDay(bad), bad)
        }
    }

    @Test
    fun relativeDatesReadLikeSpeech() {
        fun rel(d: String) = formatDate(isoDateEpochDay(d)!!, today, DateFormat.Relative)
        assertEquals("today", rel("2026-11-04"))
        assertEquals("tomorrow", rel("2026-11-05"))
        assertEquals("yesterday", rel("2026-11-03"))
        assertEquals("in 3 days", rel("2026-11-07"))
        assertEquals("in 6 days", rel("2026-11-10"))
        assertEquals("5 days ago", rel("2026-10-30"))
        assertEquals("Nov 11", rel("2026-11-11"))
        assertEquals("Jan 2, 2027", rel("2027-01-02"))
        assertEquals("Dec 25, 2025", rel("2025-12-25"))
        assertEquals("Nov 5", formatDate(isoDateEpochDay("2026-11-05")!!, today, DateFormat.Short))
    }

    @Test
    fun formatsEveryBoundDateAndShowsPastText() {
        val due = text(
            """{"type":"core://Text","value_expr":"Due ${'$'}item.due","style":"Secondary",
                "format":{"date":"Relative","past":"Overdue"}}"""
        )
        assertEquals(TextFormat(DateFormat.Relative, "Overdue"), due.format)
        assertEquals("Due tomorrow", draw(due, item("due" to "2026-11-05")))
        assertEquals("Due today", draw(due, item("due" to "2026-11-04")))
        assertEquals("Overdue", draw(due, item("due" to "2026-11-01")))
        // Not a date: drawn as it is.
        assertEquals("Due soon", draw(due, item("due" to "soon")))
        // An exact binding and an entity placeholder are formatted too; without past, a past date is relative.
        val exact = text("""{"type":"core://Text","value_expr":"${'$'}item.due","style":"Body","format":{"date":"Short"}}""")
        assertEquals("Oct 1", draw(exact, item("due" to "2026-10-01")))
        val placeholder = text(
            """{"type":"core://Text","value_expr":"Next: ${'$'}{collections://todos/1#due}","style":"Body",
                "format":{"date":"Relative"}}"""
        )
        val entities = mapOf("collections://todos/1" to item("due" to "2026-11-02"))
        assertEquals("Next: 2 days ago", draw(placeholder, entities = entities))
    }

    @Test
    fun countsTheQueryItemsThatMatchEveryWhere() {
        val left = text(
            """{"type":"core://Text","value_expr":"${'$'}count left","style":"Label",
                "count":{"query_key":"collection:todos","where":[{"field":"done","equals":"false"},
                {"field":"deleted","equals":"false"}],"zero_text":"All done"}}"""
        )
        val entities = mapOf(
            "collections://todos/1" to item("done" to false, "deleted" to false),
            "collections://todos/2" to item("done" to true, "deleted" to false),
            "collections://todos/3" to item("done" to false, "deleted" to false),
            // Deleted on this device a moment ago: not counted although the pod still lists it.
            "collections://todos/4" to item("done" to false, "deleted" to true),
        )
        val all = mapOf("collection:todos" to entities.keys.toList())
        assertEquals("2 left", draw(left, queries = all, entities = entities))
        val none = mapOf("collection:todos" to listOf("collections://todos/2"))
        assertEquals("All done", draw(left, queries = none, entities = entities))
        // Not loaded yet: no number.
        assertEquals(" left", draw(left))
        // Without where, everything counts; $count_x is not the token; a blank form value matches all.
        val total = text(
            """{"type":"core://Text","value_expr":"${'$'}count of ${'$'}count_x","style":"Body",
                "count":{"query_key":"collection:todos","where":[{"field":"done","equals":"${'$'}form.show"}]}}"""
        )
        assertEquals("4 of ${'$'}count_x", draw(total, queries = all, entities = entities, form = emptyMap()))
        assertEquals("1 of ${'$'}count_x", draw(total, queries = all, entities = entities, form = mapOf("show" to "true")))
    }

    @Test
    fun aPlainTextStillEncodesWithoutTheNewFields() {
        val plain = UiNode.Text("Hi", TextStyle.Body)
        assertEquals("""{"type":"core://Text","value_expr":"Hi","style":"Body"}""", ConstructJson.encodeNode(plain))
    }
}
