package org.genui.canvas.desktop.sim

/** One simulator script command. [command] and [arg] are what the step log records. */
sealed class SimStep(val command: String, val arg: String = "") {
    class Shot(val name: String) : SimStep("shot", name)
    class Click(val text: String) : SimStep("click", text)
    class Wait(val ms: Long) : SimStep("wait", ms.toString())
    object Idle : SimStep("idle")
    class AssertText(val text: String) : SimStep("assert-text", text)
    class AssertNoText(val text: String) : SimStep("assert-no-text", text)
    object Offline : SimStep("offline")
    object Online : SimStep("online")
    class Screen(val spec: ScreenSpec) : SimStep("screen", spec.label)

    override fun toString(): String = if (arg.isEmpty()) command else "$command $arg"
}

class ScriptParseException(message: String) : IllegalArgumentException(message)

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
        val command = entry.substringBefore(' ').substringBefore('\t').lowercase()
        val arg = entry.substring(command.length).trim()
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
                requireArg("a duration in ms").toLongOrNull()?.takeIf { it >= 0 }
                    ?: throw ScriptParseException("${where()}: 'wait' needs a non-negative integer number of ms"),
            )
            "idle" -> { noArg(); SimStep.Idle }
            "assert-text" -> SimStep.AssertText(requireArg("the expected text"))
            "assert-no-text" -> SimStep.AssertNoText(requireArg("the text that must be absent"))
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
                    "shot, click, wait, idle, assert-text, assert-no-text, offline, online, screen)",
            )
        }
    }
    return steps
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
