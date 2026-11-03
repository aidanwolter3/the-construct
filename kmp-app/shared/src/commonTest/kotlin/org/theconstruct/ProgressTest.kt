package org.theconstruct

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs

class ProgressTest {
    @Test
    fun decodesWithAndWithoutASize() {
        assertEquals(UiNode.Progress(), ConstructJson.decodeNode("""{"type":"core://Progress"}"""))
        assertEquals(UiNode.Progress(20), ConstructJson.decodeNode("""{"type":"core://Progress","size_dp":20}"""))
    }

    @Test
    fun encodesBackToTheWireForm() {
        assertEquals("""{"type":"core://Progress"}""", ConstructJson.encodeNode(UiNode.Progress()))
        assertEquals("""{"type":"core://Progress","size_dp":20}""", ConstructJson.encodeNode(UiNode.Progress(20)))
    }

    @Test
    fun decodesInsideAConditionalGroupHeader() {
        val header = ConstructJson.decodeNode(
            """{"type":"core://Row","spacing_dp":8,"children":[
                {"type":"core://Text","value_expr":"${'$'}item.section","style":"Label"},
                {"type":"core://Conditional","value":"${'$'}item.section","equals":"Unsorted",
                 "content":{"type":"core://Progress"}}]}"""
        )
        val row = assertIs<UiNode.Row>(header)
        val conditional = assertIs<UiNode.Conditional>(row.children[1])
        assertEquals("Unsorted", conditional.equals)
        assertEquals(UiNode.Progress(), conditional.content)
    }
}
