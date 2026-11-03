package org.theconstruct.desktop.sim

import org.theconstruct.ServerUrls

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
    /**
     * Queues [paths] (image files; relative ones resolve against the working directory) for the next photo picker a
     * `core://MediaPicker` opens, e.g. `pick fixtures/pixelfed/media/a.png; click tag:media-picker-add:photos`.
     */
    class Pick(val paths: List<String>) : SimStep("pick", paths.joinToString(" "))

    // Hamburger menu: each waits for idle and for the apps calls, and fails with a hint when its control is missing.
    object OpenMenu : SimStep("open-menu")
    object CloseMenu : SimStep("close-menu")
    /** Opens the menu's Settings page and waits for its list of sign-ins. */
    object OpenSettings : SimStep("open-settings")
    /** [app]: an app id (`pixelfed_home`) or its title (`Pixelfed`). */
    class OpenApp(val app: String) : SimStep("open-app", app)
    class OpenHistory(val app: String) : SimStep("open-history", app)
    /** [version] null = "Latest". */
    class SelectVersion(val version: Long?) : SimStep("select-version", version?.toString() ?: "latest")
    /** [server]: `staging`, `emulator`, `usb` or an http(s) URL. */
    class SetServer(val server: String) : SimStep("set-server", server)
    /** Fixture mode only: the fake pod publishes a new newest version of the app on screen. */
    class PublishVersion(val note: String) : SimStep("publish-version", note)

    // Notifications (D52).
    /** Fixture mode only: a live notification from [app]'s [channel] (fires the listener, bumps the unread count). */
    class Notify(val app: String, val channel: String, val title: String) : SimStep("notify", "$app $channel $title")
    object OpenInbox : SimStep("open-inbox")
    object CloseInbox : SimStep("close-inbox")
    /** Clicks the inbox row containing [text] (opens its app and target). */
    class ClickNotification(val text: String) : SimStep("click-notification", text)
    object OpenNotificationSettings : SimStep("open-notification-settings")
    /** [channel]: `<app>.<channel id>` (e.g. `pixelfed_home.likes`). */
    class ToggleChannel(val channel: String) : SimStep("toggle-channel", channel)

    override fun toString(): String = if (arg.isEmpty()) command else "$command $arg"
}

/** The `set-server` presets (the menu's `server-<key>` rows in the simulator). */
val SERVER_PRESETS = listOf("staging", "emulator", "usb")

class ScriptParseException(message: String) : IllegalArgumentException(message)

/** Upper bound for `wait`, 10 minutes. */
const val MAX_WAIT_MS = 600_000L

/** Default timeout of `wait-images`. */
const val DEFAULT_WAIT_IMAGES_MS = 10_000L

/**
 * Rejects steps that [mode] can't run: `offline`/`online`/`publish-version`/`notify` need the fixture mode's fake
 * engine.
 */
fun validateStepsForMode(steps: List<SimStep>, mode: String) {
    if (mode == "fixture") return
    val i = steps.indexOfFirst {
        it == SimStep.Offline || it == SimStep.Online || it is SimStep.PublishVersion || it is SimStep.Notify
    }
    if (i >= 0) {
        throw ScriptParseException("step ${i + 1} '${steps[i].command}' is not supported in $mode mode (fixture mode only)")
    }
}

/**
 * Parses a simulator script (commands: README "Simulation mode"; `pick <path> [<path>…]` queues image files for the
 * next photo picker, see [SimStep.Pick]). Commands are separated by `;` or newlines; each is trimmed, blank
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
            "pick" -> SimStep.Pick(
                splitPaths(requireArg("one or more file paths"))
                    ?: throw ScriptParseException("${where()}: 'pick' needs file paths (quote a path with spaces: \"my photo.jpg\")"),
            )
            "open-menu" -> { noArg(); SimStep.OpenMenu }
            "close-menu" -> { noArg(); SimStep.CloseMenu }
            "open-settings" -> { noArg(); SimStep.OpenSettings }
            "open-app" -> SimStep.OpenApp(requireArg("an app id or title (e.g. pixelfed_home or Pixelfed)"))
            "open-history" -> SimStep.OpenHistory(requireArg("an app id or title (e.g. pixelfed_home or Pixelfed)"))
            "select-version" -> {
                val a = requireArg("a version number or 'latest'")
                SimStep.SelectVersion(
                    if (a.equals("latest", ignoreCase = true)) {
                        null
                    } else {
                        a.removePrefix("v").toLongOrNull()?.takeIf { it >= 1 }
                            ?: throw ScriptParseException("${where()}: 'select-version' needs a version number >= 1 (e.g. 2 or v2) or 'latest'")
                    },
                )
            }
            "set-server" -> {
                val a = requireArg("staging, emulator, usb or an http(s)://host[:port] URL")
                if (a.lowercase() !in SERVER_PRESETS && ServerUrls.parse(a) == null) {
                    throw ScriptParseException("${where()}: 'set-server' needs staging, emulator, usb or an http(s)://host[:port] URL")
                }
                SimStep.SetServer(a)
            }
            "publish-version" -> SimStep.PublishVersion(requireArg("a note (the changelog line)"))
            "notify" -> {
                val parts = requireArg("'<app_id> <channel> <title>'").split(Regex("\\s+"), limit = 3)
                if (parts.size < 3 || parts[2].isBlank()) {
                    throw ScriptParseException("${where()}: 'notify' needs '<app_id> <channel> <title>'")
                }
                SimStep.Notify(parts[0], parts[1], parts[2].trim())
            }
            "open-inbox" -> { noArg(); SimStep.OpenInbox }
            "close-inbox" -> { noArg(); SimStep.CloseInbox }
            "click-notification" -> SimStep.ClickNotification(requireArg("text of the notification to click"))
            "open-notification-settings" -> { noArg(); SimStep.OpenNotificationSettings }
            "toggle-channel" -> {
                val a = requireArg("'<app_id>.<channel>' (e.g. pixelfed_home.likes)")
                if (!a.contains('.') || a.startsWith('.') || a.endsWith('.') || a.any { it.isWhitespace() }) {
                    throw ScriptParseException("${where()}: 'toggle-channel' needs '<app_id>.<channel>' (e.g. pixelfed_home.likes)")
                }
                SimStep.ToggleChannel(a)
            }
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
                    "scroll-to, type, back, pick, open-menu, close-menu, open-settings, open-app, open-history, select-version, set-server, " +
                    "publish-version, notify, open-inbox, close-inbox, click-notification, open-notification-settings, " +
                    "toggle-channel, offline, online, screen)",
            )
        }
    }
    return steps
}

/** Splits a `pick` argument into paths: words, or double-quoted paths with spaces. Null when a quote is unclosed. */
internal fun splitPaths(arg: String): List<String>? {
    val out = mutableListOf<String>()
    var i = 0
    val a = arg.trim()
    while (i < a.length) {
        when {
            a[i].isWhitespace() -> i++
            a[i] == '"' -> {
                val end = a.indexOf('"', i + 1)
                if (end < 0) return null
                a.substring(i + 1, end).takeIf { it.isNotEmpty() }?.let(out::add)
                i = end + 1
            }
            else -> {
                var end = i
                while (end < a.length && !a[end].isWhitespace()) end++
                out += a.substring(i, end)
                i = end
            }
        }
    }
    return out.takeIf { it.isNotEmpty() }
}

/**
 * Splits a `type` argument into target and text: a target in double quotes may contain spaces (`"Search posts" cats`),
 * otherwise it is the first word (`tag:server-custom-url http://10.0.0.5:8080`). Null when either part is missing.
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
