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
    fun mutationTernary() {
        val liked = JsonObject(mapOf("is_liked" to JsonPrimitive(true), "like_count" to JsonPrimitive(5), "n" to JsonPrimitive(1)))
        val unliked = JsonObject(mapOf("is_liked" to JsonPrimitive(false), "like_count" to JsonPrimitive(5)))
        val expr = "entity.is_liked ? entity.like_count - 1 : entity.like_count + 1"
        assertEquals(JsonPrimitive(4), FakeEngineBridge.evalMutation(expr, liked))
        assertEquals(JsonPrimitive(6), FakeEngineBridge.evalMutation(expr, unliked))
        assertEquals(JsonPrimitive("b"), FakeEngineBridge.evalMutation("!entity.is_liked ? \"a\" : \"b\"", liked))
        assertEquals(JsonPrimitive("x:y?"), FakeEngineBridge.evalMutation("entity.is_liked ? {\"a\":1} : \"x:y?\"", unliked))
        assertEquals(JsonPrimitive(false), FakeEngineBridge.evalMutation("entity.is_liked ? !entity.is_liked : 0", liked))
        assertEquals(JsonPrimitive("a?b:c"), FakeEngineBridge.evalMutation("\"a?b:c\"", liked))
        // Same error cases as expr.rs `ternary_errors`.
        for (bad in listOf(
            "entity.missing ? 1 : 2", "!entity.missing ? 1 : 2", "entity.n ? 1 : 2", "entity.is_liked ? 1",
            "entity.is_liked ? : 2", "entity.is_liked ? 1 :", "entity.is_liked ? entity.is_liked ? 1 : 2 : 3",
            "entity.is_liked ? 1 : entity.is_liked ? 2 : 3", "entity.is_liked ? 1 * 2 : 3", "true ? 1 : 2",
        )) {
            assertFailsWith<IllegalArgumentException>(bad) { FakeEngineBridge.evalMutation(bad, liked) }
        }
    }

    @Test
    fun unknownCommandWhoseLowercaseIsLongerIsAParseError() {
        assertFailsWith<ScriptParseException> { parseScript("İ") }
    }

    @Test
    fun imageAndGestureCommands() {
        val steps = parseScript("wait-images; wait-images 2500; swipe-left tag:carousel:pixelfed://posts/1; swipe-right Alt text; scroll-to Legs")
        assertEquals(DEFAULT_WAIT_IMAGES_MS, assertIs<SimStep.WaitImages>(steps[0]).timeoutMs)
        assertEquals(2500L, assertIs<SimStep.WaitImages>(steps[1]).timeoutMs)
        val left = assertIs<SimStep.Swipe>(steps[2])
        assertEquals("left", left.direction)
        assertEquals("tag:carousel:pixelfed://posts/1", left.target)
        assertEquals("swipe-left", left.command)
        assertEquals("right", assertIs<SimStep.Swipe>(steps[3]).direction)
        assertEquals("Legs", assertIs<SimStep.ScrollTo>(steps[4]).target)
        assertFailsWith<ScriptParseException> { parseScript("wait-images soon") }
        assertFailsWith<ScriptParseException> { parseScript("swipe-left") }
        assertFailsWith<ScriptParseException> { parseScript("scroll-to") }
        assertFailsWith<ScriptParseException> { parseScript("swipe-up x") }
    }

    @Test
    fun typeTakesATargetThenText() {
        val steps = parseScript("type tag:agent-prompt Hide post captions; type \"Describe a change\" media first ")
        val a = assertIs<SimStep.Type>(steps[0])
        assertEquals("tag:agent-prompt", a.target)
        assertEquals("Hide post captions", a.text)
        assertEquals("type", a.command)
        val b = assertIs<SimStep.Type>(steps[1])
        assertEquals("Describe a change", b.target)
        assertEquals("media first", b.text)
        assertFailsWith<ScriptParseException> { parseScript("type") }
        assertFailsWith<ScriptParseException> { parseScript("type tag:agent-prompt") }
        assertFailsWith<ScriptParseException> { parseScript("type \"unterminated text") }
        assertFailsWith<ScriptParseException> { parseScript("type \"\" text") }
    }
}
