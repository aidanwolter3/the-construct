package org.genui.canvas.desktop.sim

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
import kotlin.test.assertTrue

class SimScriptTest {
    @Test
    fun parsesCommandsSeparatedBySemicolonsAndNewlines() {
        val steps = parseScript(
            """
            # a comment
            shot start; click Increment ;; wait 250
            idle
            assert-text Count: 2; assert-no-text Count: 3
            offline; online; screen tablet; screen 320x640
            """.trimIndent(),
        )
        assertEquals(
            listOf(
                "shot start", "click Increment", "wait 250", "idle", "assert-text Count: 2",
                "assert-no-text Count: 3", "offline", "online", "screen tablet", "screen 320x640",
            ),
            steps.map { it.toString() },
        )
        assertEquals(250L, assertIs<SimStep.Wait>(steps[2]).ms)
        val custom = assertIs<SimStep.Screen>(steps[9]).spec
        assertEquals(ScreenSpec("320x640", 320, 640, 2f), custom)
    }

    @Test
    fun emptyScriptHasNoSteps() {
        assertEquals(emptyList(), parseScript("  ; # nothing\n"))
    }

    @Test
    fun unknownCommandIsAnError() {
        val e = assertFailsWith<ScriptParseException> { parseScript("shot a; tap Increment") }
        assertTrue(e.message!!.contains("unknown command 'tap'"), e.message)
    }

    @Test
    fun badArgumentsAreErrors() {
        assertFailsWith<ScriptParseException> { parseScript("shot") }
        assertFailsWith<ScriptParseException> { parseScript("wait soon") }
        assertFailsWith<ScriptParseException> { parseScript("wait -5") }
        assertFailsWith<ScriptParseException> { parseScript("wait 600001") }
        parseScript("wait 600000")
        assertFailsWith<ScriptParseException> { parseScript("idle now") }
        assertFailsWith<ScriptParseException> { parseScript("screen huge") }
        assertFailsWith<ScriptParseException> { parseScript("wait-text 5000") }
        assertFailsWith<ScriptParseException> { parseScript("wait-text soon Count: 1") }
        assertFailsWith<ScriptParseException> { parseScript("wait-text 600001 Count: 1") }
    }

    @Test
    fun waitTextTakesTimeoutThenText() {
        val step = assertIs<SimStep.WaitText>(parseScript("wait-text 15000 Count: 3").single())
        assertEquals(15000L, step.timeoutMs)
        assertEquals("Count: 3", step.text)
        assertEquals("wait-text 15000 Count: 3", step.toString())
    }

    @Test
    fun screenPresets() {
        assertEquals(1170, ScreenSpec.parse("phone").widthPx(3f))
        assertEquals(ScreenSpec("tablet", 820, 1180, 2f), ScreenSpec.parse("Tablet"))
        assertEquals(ScreenSpec("desktop", 1280, 800, 2f), ScreenSpec.parse("desktop"))
    }

    @Test
    fun mutationExpressionSubset() {
        val e = JsonObject(mapOf("count" to JsonPrimitive(4), "liked" to JsonPrimitive(false)))
        assertEquals(JsonPrimitive(5), FakeEngineBridge.evalMutation("entity.count + 1", e))
        assertEquals(JsonPrimitive(1), FakeEngineBridge.evalMutation("entity.count-3", e))
        assertEquals(JsonPrimitive(1), FakeEngineBridge.evalMutation("entity.missing + 1", e))
        assertEquals(JsonPrimitive(true), FakeEngineBridge.evalMutation("!entity.liked", e))
        assertEquals(JsonPrimitive(4), FakeEngineBridge.evalMutation("entity.count", e))
        assertEquals(JsonPrimitive(0), FakeEngineBridge.evalMutation("0", e))
        assertFailsWith<IllegalArgumentException> { FakeEngineBridge.evalMutation("entity.count * 2", e) }
        // Unsupported forms from expr.rs's `unsupported` test, plus unquoted tokens that
        // kotlinx's parseToJsonElement would otherwise accept as literals.
        for (expr in listOf(
            "", "entity.count + 1.5", "entity.count + entity.count", "entity.count + -1", "entity.",
            "entity.a.b", "foo.count", "!true", "size(entity.list)", "foo", "NaN", "'x'",
        )) {
            assertFailsWith<IllegalArgumentException>(expr) { FakeEngineBridge.evalMutation(expr, e) }
        }
        assertEquals(JsonPrimitive("x"), FakeEngineBridge.evalMutation("\"x\"", e))
    }

    @Test
    fun unknownCommandWhoseLowercaseIsLongerIsAParseError() {
        assertFailsWith<ScriptParseException> { parseScript("İ") }
    }
}
