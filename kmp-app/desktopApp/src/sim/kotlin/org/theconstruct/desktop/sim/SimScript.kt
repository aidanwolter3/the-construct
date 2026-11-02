package org.theconstruct.desktop.sim

/** One simulator script command. [command] and [arg] are what the step log records. */
sealed class SimStep(val command: String, val arg: String = "") {
    class Shot(val name: String) : SimStep("shot", name)
    class Click(val text: String) : SimStep("click", text)
    class Wait(val ms: Long) : SimStep("wait", ms.toString())
    object Idle : SimStep("idle")
    class AssertText(val text: String) : SimStep("assert-text", text)
    class AssertNoText(val text: String) : SimStep("assert-no-text", text)
    class WaitText(val timeoutMs: Long, val text: String) : SimStep("wait-text", "$timeoutMs $text")
    object Offline : SimStep("offline")
    object Online : SimStep("online")
    class Screen(val spec: ScreenSpec) : SimStep("screen", spec.label)
    class WaitImages(val timeoutMs: Long) : SimStep("wait-images", timeoutMs.toString())
    /** Swipes the node [target] selects (see README) towards [direction]: left shows the next carousel page. */
    class Swipe(val direction: String, val target: String) : SimStep("swipe-$direction", target)
    class ScrollTo(val target: String) : SimStep("scroll-to", target)
    /** Replaces the text of the text field [target] selects with [text]. */
    class Type(val target: String, val text: String) : SimStep("type", "$target $text")
    /** A system Back press (Android's Back; the desktop stand-in [org.theconstruct.BackPresses]). */
    object Back : SimStep("back")

    // Layout browser (DECISIONS D30): each waits for idle and for the catalog calls, and fails with a hint when its
    // control is not on screen.
    object OpenLayouts : SimStep("open-layouts")
    /** [id]: a full layout id (`pixelfed_demo.editorial`) or its last segment (`editorial`). */
    class SelectLayout(val id: String) : SimStep("select-layout", id)
    class SelectVersion(val version: Int) : SimStep("select-version", version.toString())
    object UseLayout : SimStep("use-layout")
    object RestoreVersion : SimStep("restore-version")
    object Confirm : SimStep("confirm")
    object CancelConfirm : SimStep("cancel")
    object Undo : SimStep("undo")
    object CloseLayouts : SimStep("close-layouts")

    override fun toString(): String = if (arg.isEmpty()) command else "$command $arg"
}

class ScriptParseException(message: String) : IllegalArgumentException(message)

/** Upper bound for `wait`, 10 minutes. */
const val MAX_WAIT_MS = 600_000L

/** Default timeout of `wait-images`. */
const val DEFAULT_WAIT_IMAGES_MS = 10_000L

/** Rejects steps that [mode] can't run: `offline`/`online` need the fixture mode's fake engine. */
fun validateStepsForMode(steps: List<SimStep>, mode: String) {
    if (mode == "fixture") return
    val i = steps.indexOfFirst { it == SimStep.Offline || it == SimStep.Online }
    if (i >= 0) {
        throw ScriptParseException("step ${i + 1} '${steps[i].command}' is not supported in $mode mode (fixture mode only)")
    }
}

/**
 * Parses a simulator script. Commands are separated by `;` or newlines; each is trimmed, blank
 * entries are ignored and entries starting with `#` are comments. The first word is the command
 * and the rest of the entry (trimmed) is its argument, so `click Count: 2` clicks "Count: 2".
 * A `;` cannot appear inside an argument.
 */
fun parseScript(script: String): List<SimStep> {
    val steps = mutableListOf<SimStep>()
    script.split(';', '\n').forEachIndexed { index, raw ->
        val entry = raw.trim()
        if (entry.isEmpty() || entry.startsWith("#")) return@forEachIndexed
        val rawCommand = entry.substringBefore(' ').substringBefore('\t')
        val command = rawCommand.lowercase()
        val arg = entry.substring(rawCommand.length).trim()
        fun where() = "command #${index + 1} '$entry'"
        fun requireArg(what: String): String =
            arg.ifEmpty { throw ScriptParseException("${where()}: '$command' needs $what") }
        fun noArg() {
            if (arg.isNotEmpty()) throw ScriptParseException("${where()}: '$command' takes no argument")
        }
        steps += when (command) {
            "shot" -> SimStep.Shot(requireArg("a name"))
            "click" -> SimStep.Click(requireArg("the text to click"))
            "wait" -> SimStep.Wait(
                requireArg("a duration in ms").toLongOrNull()?.takeIf { it in 0..MAX_WAIT_MS }
                    ?: throw ScriptParseException("${where()}: 'wait' needs an integer number of ms in 0..$MAX_WAIT_MS"),
            )
            "idle" -> { noArg(); SimStep.Idle }
            "assert-text" -> SimStep.AssertText(requireArg("the expected text"))
            "assert-no-text" -> SimStep.AssertNoText(requireArg("the text that must be absent"))
            "wait-text" -> {
                val a = requireArg("a timeout in ms and the text")
                val ms = a.substringBefore(' ').toLongOrNull()?.takeIf { it in 0..MAX_WAIT_MS }
                val text = a.substringAfter(' ', "").trim()
                if (ms == null || text.isEmpty()) {
                    throw ScriptParseException("${where()}: 'wait-text' needs '<ms in 0..$MAX_WAIT_MS> <text>'")
                }
                SimStep.WaitText(ms, text)
            }
            "wait-images" -> SimStep.WaitImages(
                if (arg.isEmpty()) DEFAULT_WAIT_IMAGES_MS else arg.toLongOrNull()?.takeIf { it in 0..MAX_WAIT_MS }
                    ?: throw ScriptParseException("${where()}: 'wait-images' takes an optional timeout in ms in 0..$MAX_WAIT_MS"),
            )
            "swipe-left", "swipe-right" -> SimStep.Swipe(command.removePrefix("swipe-"), requireArg("a target (text, content description or tag:<testTag>)"))
            "scroll-to" -> SimStep.ScrollTo(requireArg("a target (text, content description or tag:<testTag>)"))
            "type" -> {
                val a = requireArg("a target and the text to type")
                val (target, text) = splitTypeArg(a)
                    ?: throw ScriptParseException(
                        "${where()}: 'type' needs '<target> <text>' (target: tag:<testTag>, one word, or \"quoted text\")",
                    )
                SimStep.Type(target, text)
            }
            "back" -> { noArg(); SimStep.Back }
            "open-layouts" -> { noArg(); SimStep.OpenLayouts }
            "select-layout" -> SimStep.SelectLayout(
                requireArg("a layout id (e.g. pixelfed_demo.editorial or editorial)").also {
                    if (it.contains(' ')) throw ScriptParseException("${where()}: a layout id has no spaces")
                },
            )
            "select-version" -> SimStep.SelectVersion(
                requireArg("a version number").removePrefix("v").toIntOrNull()?.takeIf { it >= 1 }
                    ?: throw ScriptParseException("${where()}: 'select-version' needs a version number >= 1 (e.g. 2 or v2)"),
            )
            "use-layout" -> { noArg(); SimStep.UseLayout }
            "restore-version" -> { noArg(); SimStep.RestoreVersion }
            "confirm" -> { noArg(); SimStep.Confirm }
            "cancel" -> { noArg(); SimStep.CancelConfirm }
            "undo" -> { noArg(); SimStep.Undo }
            "close-layouts" -> { noArg(); SimStep.CloseLayouts }
            "offline" -> { noArg(); SimStep.Offline }
            "online" -> { noArg(); SimStep.Online }
            "screen" -> SimStep.Screen(
                try {
                    ScreenSpec.parse(requireArg("WxH or a preset (phone, tablet, desktop)"))
                } catch (e: IllegalArgumentException) {
                    throw ScriptParseException("${where()}: ${e.message}")
                },
            )
            else -> throw ScriptParseException(
                "${where()}: unknown command '$command' (expected one of: " +
                    "shot, click, wait, idle, assert-text, assert-no-text, wait-text, wait-images, swipe-left, swipe-right, " +
                    "scroll-to, type, back, open-layouts, select-layout, select-version, use-layout, restore-version, " +
                    "confirm, cancel, undo, close-layouts, offline, online, screen)",
            )
        }
    }
    return steps
}

/**
 * Splits a `type` argument into target and text: a target in double quotes may contain spaces (`"Search posts" cats`),
 * otherwise it is the first word (`tag:agent-prompt Hide post captions`). Null when either part is missing.
 */
internal fun splitTypeArg(arg: String): Pair<String, String>? {
    val a = arg.trim()
    val (target, rest) = if (a.startsWith('"')) {
        val end = a.indexOf('"', 1)
        if (end < 0) return null
        a.substring(1, end) to a.substring(end + 1)
    } else {
        val end = a.indexOfFirst { it.isWhitespace() }
        if (end < 0) return null
        a.substring(0, end) to a.substring(end)
    }
    val text = rest.trim()
    return if (target.isEmpty() || text.isEmpty()) null else target to text
}

/** A virtual screen in dp. [defaultDensity] applies unless a density is given explicitly. */
data class ScreenSpec(val label: String, val widthDp: Int, val heightDp: Int, val defaultDensity: Float) {
    fun widthPx(density: Float): Int = Math.round(widthDp * density)
    fun heightPx(density: Float): Int = Math.round(heightDp * density)

    companion object {
        val PRESETS: Map<String, ScreenSpec> = listOf(
            ScreenSpec("phone", 390, 844, 3f),
            ScreenSpec("tablet", 820, 1180, 2f),
            ScreenSpec("desktop", 1280, 800, 2f),
        ).associateBy { it.label }

        /** Density used for an explicit `WxH` screen. */
        const val CUSTOM_DEFAULT_DENSITY = 2f

        private val WXH = Regex("""(\d+)\s*[xX]\s*(\d+)""")

        fun parse(spec: String): ScreenSpec {
            val s = spec.trim()
            PRESETS[s.lowercase()]?.let { return it }
            val m = WXH.matchEntire(s)
                ?: throw IllegalArgumentException("bad screen '$spec': expected WxH in dp (e.g. 390x844) or one of ${PRESETS.keys}")
            val (w, h) = m.destructured
            val wi = w.toInt()
            val hi = h.toInt()
            require(wi in 1..10000 && hi in 1..10000) { "bad screen '$spec': width and height must be 1..10000 dp" }
            return ScreenSpec("${wi}x$hi", wi, hi, CUSTOM_DEFAULT_DENSITY)
        }
    }
}
