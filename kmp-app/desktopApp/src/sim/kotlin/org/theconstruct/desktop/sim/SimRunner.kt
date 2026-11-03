package org.theconstruct.desktop.sim

import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asSkiaBitmap
import androidx.compose.ui.test.ComposeTimeoutException
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.SemanticsNodeInteraction
import androidx.compose.ui.test.SemanticsNodeInteractionCollection
import androidx.compose.ui.test.SkikoComposeUiTest
import androidx.compose.ui.test.hasClickAction
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextClearance
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.swipeLeft
import androidx.compose.ui.test.swipeRight
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.click
import androidx.compose.ui.test.performMouseInput
import androidx.compose.ui.test.runSkikoComposeUiTest
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.unit.Density
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.hasAnyAncestor
import org.theconstruct.BackPresses
import org.theconstruct.ConstructApp
import org.theconstruct.ConstructController
import org.theconstruct.ImageLoads
import org.theconstruct.LayoutCalls
import org.theconstruct.MediaCalls
import org.theconstruct.LocalBackPresses
import org.theconstruct.MenuState
import org.theconstruct.ServerChoice
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.performSemanticsAction
import org.theconstruct.ServerOption
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import org.jetbrains.skia.EncodedImageFormat
import org.jetbrains.skia.Image
import java.io.File
import kotlin.time.Duration.Companion.minutes

data class StepResult(
    val index: Int,
    val command: String,
    val arg: String,
    val ok: Boolean,
    val message: String,
    val file: File?,
    val elapsedMs: Long,
) {
    val status: String get() = if (ok) "ok" else "fail"

    fun logLine(): String {
        val cmd = if (arg.isEmpty()) command else "$command $arg"
        val tail = buildString {
            if (file != null) append(" -> ${file.name}")
            if (message.isNotEmpty()) append(" : $message")
        }
        return "[%02d] %-4s %s%s (%d ms)".format(index, status, cmd, tail, elapsedMs)
    }
}

data class SimResult(val steps: List<StepResult>, val outDir: File) {
    val ok: Boolean get() = steps.all { it.ok }
}

private class StepFailure(message: String) : Exception(message)

/**
 * Runs [steps] against [ConstructApp] rendered offscreen by the Compose UI test harness
 * (`runSkikoComposeUiTest`, a Skia raster surface; no window, works with java.awt.headless=true).
 *
 * The virtual screen is [screen] in dp at [density] (null = the screen's default density), so
 * screenshots are `widthDp * density` x `heightDp * density` pixels. A `screen` step restarts the
 * harness at the new size; the controller (and so engine state) carries over, Compose UI state
 * such as scroll position does not. [setOnline] is null when connectivity can't be simulated
 * (live mode); `offline`/`online` then fail. The first failing step stops the run and its screen
 * is saved as `NN-FAILED.png` (best effort).
 */
@OptIn(ExperimentalTestApi::class)
class SimRunner(
    private val controller: ConstructController,
    private val outDir: File,
    private val screen: ScreenSpec,
    private val density: Float? = null,
    private val setOnline: ((Boolean) -> Unit)? = null,
    private val modeName: String = "fixture",
    private val textTimeoutMs: Long = 2_000,
    private val log: (String) -> Unit = ::println,
    /** Installs the simulator's Coil ImageLoader for [modeName] ([SimImages]) at the start of [run]. */
    private val installImageLoader: Boolean = true,
    /** Fixture mode: the fake pod publishes a new version of the app on screen (`publish-version`); returns its number. */
    private val publishVersion: ((String) -> Long)? = null,
    /** The server the menu shows at first; `set-server` records the choice (the simulator never switches servers). */
    server: String = SIM_SERVERS.first().url,
    /** The photo picker `core://MediaPicker` opens; `pick` queues its files. */
    val mediaPicker: SimMediaPicker = SimMediaPicker(log),
    /** Fixture mode: the fake pod finishes the posts in flight ([FakeEngineBridge.settle]); run by `wait` and `idle`. */
    private val settle: (() -> Int)? = null,
) {
    /** The server the menu shows: the last `set-server` choice. */
    var serverUrl: String by mutableStateOf(server)
        private set

    /** Every server chosen in the menu, in order. */
    val serverSelections = mutableListOf<String>()

    private val serverChoice: ServerChoice
        get() = ServerChoice(serverUrl, SIM_SERVERS) { url ->
            log("[sim] server picked: $url (recorded; the simulator keeps its engine)")
            serverSelections += url
            serverUrl = url
        }

    private var backPresses: BackPresses? = null

    fun run(steps: List<SimStep>): SimResult {
        outDir.mkdirs()
        if (installImageLoader) SimImages.install(modeName)
        val results = mutableListOf<StepResult>()
        var i = 0
        var current = screen
        var failed = false
        while (i < steps.size && !failed) {
            val d = density ?: current.defaultDensity
            val w = current.widthPx(d)
            val h = current.heightPx(d)
            log("[sim] screen ${current.label}: ${current.widthDp}x${current.heightDp} dp @ ${d}x = ${w}x$h px")
            var harnessStart = System.nanoTime()
            try {
                runSkikoComposeUiTest(
                    size = Size(w.toFloat(), h.toFloat()),
                    density = Density(d),
                    testTimeout = 30.minutes,
                ) {
                    // A stand-in for the system Back (the `back` command and the layout browser's levels).
                    val backPresses = BackPresses()
                    this@SimRunner.backPresses = backPresses
                    setContent {
                        CompositionLocalProvider(LocalBackPresses provides backPresses) { ConstructApp(controller, server = serverChoice, mediaPicker = mediaPicker) }
                    }
                    waitForIdle()
                    while (i < steps.size) {
                        val step = steps[i]
                        val start = System.nanoTime()
                        val index = i + 1
                        i++
                        if (step is SimStep.Screen) {
                            current = step.spec
                            results += record(StepResult(index, step.command, step.arg, true, "restarting harness", null, ms(start)))
                            return@runSkikoComposeUiTest
                        }
                        harnessStart = start
                        val r = try {
                            val (msg, file) = execute(step, index)
                            StepResult(index, step.command, step.arg, true, msg, file, ms(start))
                        } catch (e: StepFailure) {
                            StepResult(index, step.command, step.arg, false, e.message.orEmpty(), null, ms(start))
                        } catch (e: Throwable) {
                            StepResult(index, step.command, step.arg, false, "${e::class.simpleName}: ${e.message}", null, ms(start))
                        }
                        if (!r.ok) {
                            results += record(r.copy(file = captureFailure(index)))
                            failed = true
                            return@runSkikoComposeUiTest
                        }
                        results += record(r)
                    }
                }
            } catch (e: Throwable) {
                // The harness itself failed (e.g. composition crashed). Blame the last step started.
                failed = true
                val idx = i.coerceAtLeast(1)
                val step = steps[idx - 1]
                val msg = "harness error: ${e::class.simpleName}: ${e.message}"
                val prev = results.lastOrNull()
                if (prev != null && prev.index == idx) {
                    results[results.lastIndex] = prev.copy(ok = false, message = listOf(prev.message, msg).filter { it.isNotEmpty() }.joinToString("; "))
                    log("[sim] $msg")
                } else {
                    results += record(StepResult(idx, step.command, step.arg, false, msg, null, ms(harnessStart)))
                }
            }
        }
        val result = SimResult(results, outDir)
        writeLogs(result)
        return result
    }

    private fun record(r: StepResult): StepResult = r.also { log(it.logLine()) }

    private fun SkikoComposeUiTest.execute(step: SimStep, index: Int): Pair<String, File?> {
        when (step) {
            is SimStep.Shot -> {
                // Apps calls first; then images still loading after SHOT_IMAGE_WAIT_MS are captured as placeholders
                // (not a failure).
                awaitLayoutCalls()
                val settled = awaitImages(SHOT_IMAGE_WAIT_MS)
                val image = onRoot().captureToImage()
                val file = File(outDir, "%02d-%s.png".format(index, sanitize(step.name)))
                file.writeBytes(encodePng(image))
                val note = if (settled) "" else " (${ImageLoads.inFlight.value} image(s) still loading)"
                return "${image.width}x${image.height}$note" to file
            }
            is SimStep.WaitImages -> {
                if (!awaitImages(step.timeoutMs)) {
                    throw StepFailure("timed out after ${step.timeoutMs} ms with ${ImageLoads.inFlight.value} image(s) still loading")
                }
                return "" to null
            }
            is SimStep.Swipe -> {
                waitForIdle()
                val (node, how) = findTarget(step.target)
                node.performTouchInput { if (step.direction == "left") swipeLeft() else swipeRight() }
                waitForIdle()
                return how to null
            }
            is SimStep.ScrollTo -> {
                waitForIdle()
                val (node, how) = findTarget(step.target)
                node.performScrollTo()
                waitForIdle()
                return how to null
            }
            is SimStep.Type -> {
                waitForIdle()
                val (node, how) = findTarget(step.target)
                node.performTextClearance()
                node.performTextInput(step.text)
                waitForIdle()
                return how to null
            }
            is SimStep.Click -> {
                waitForIdle()
                val (node, found) = findTarget(step.text)
                val how = clickNode(node, found, step.text)
                // A click on a media picker's add tile picks and stages off the UI thread.
                awaitMediaCalls()
                return how to null
            }
            is SimStep.Wait -> {
                Thread.sleep(step.ms)
                mainClock.advanceTimeBy(step.ms)
                waitForIdle()
                return settled() to null
            }
            SimStep.Idle -> {
                waitForIdle()
                return settled() to null
            }
            is SimStep.Pick -> {
                val files = step.paths.map { File(it).absoluteFile }
                val missing = files.filter { !it.isFile }
                if (missing.isNotEmpty()) throw StepFailure("no such file: ${missing.joinToString()}")
                mediaPicker.enqueue(files.map { it.path })
                return "${files.size} file(s) queued for the next photo picker" to null
            }
            is SimStep.AssertText -> {
                awaitCondition("text '${step.text}' to be present") { hasAnyText(step.text) }
                return "" to null
            }
            is SimStep.AssertNoText -> {
                awaitCondition("text '${step.text}' to be absent") { !hasAnyText(step.text) }
                return "" to null
            }
            is SimStep.WaitText -> {
                awaitCondition("text '${step.text}' to be present", step.timeoutMs) { hasAnyText(step.text) }
                return "" to null
            }
            SimStep.Offline, SimStep.Online -> {
                val set = setOnline ?: throw StepFailure("'${step.command}' is not supported in $modeName mode")
                set(step == SimStep.Online)
                waitForIdle()
                return "" to null
            }
            SimStep.Back -> {
                waitForIdle()
                val presses = backPresses ?: throw StepFailure("Back is not available")
                if (!presses.press()) throw StepFailure("nothing on screen handles Back (the menu is closed)")
                waitForIdle()
                return "" to null
            }
            SimStep.OpenMenu -> {
                if (!menuShown()) clickTag("menu-button", "the menu button is not on screen")
                awaitMenu()
                awaitLayoutCalls()
                if (hasTag("apps-error")) return "error shown: ${textUnder("apps-error")}" to null
                return (if (hasTag("version-picker")) "version picker" else "${appTags().size} app(s)") to null
            }
            SimStep.CloseMenu -> {
                if (!menuShown()) throw StepFailure("the menu is not open")
                if (hasTag("version-picker")) clickTag("menu-back", "")
                clickTag("menu-close", "")
                awaitCondition("the menu to close", LAYOUT_TIMEOUT_MS) { !menuShown() && !controller.menu.open }
                return "" to null
            }
            is SimStep.OpenApp -> {
                val tag = appTag(step.app)
                val id = tag.removePrefix("app-")
                if (id != controller.currentAppId() && !controller.canOpenApps) {
                    throw StepFailure("cannot open '$id': ${MenuState.OPEN_APP_UNAVAILABLE} (its row is disabled)")
                }
                clickTag(tag, "")
                awaitLayoutCalls()
                if (hasTag("app-error")) throw StepFailure("cannot open '$id': ${textUnder("app-error")}")
                awaitCondition("app '$id' to load", LAYOUT_TIMEOUT_MS) {
                    !menuShown() && controller.currentAppId() == id && controller.state.value.workspace != null
                }
                return "opened $id" to null
            }
            is SimStep.OpenHistory -> {
                val id = appTag(step.app).removePrefix("app-")
                clickTag("app-history-$id", "")
                awaitTag("version-picker", "the version picker of '$id' did not open")
                awaitLayoutCalls()
                if (hasTag("versions-error")) return "error shown: ${textUnder("versions-error")}" to null
                return "${tagsStartingWith("version-").count { VERSION_TAG.matches(it) }} version(s) listed" to null
            }
            is SimStep.SelectVersion -> {
                requireTag("version-picker", "no version picker on screen (use open-history first)")
                val tag = step.version?.let { "version-$it" } ?: "version-latest"
                // Older versions are paged: press "Load older" until the row appears or there is no more history.
                while (!hasTag(tag) && hasTag("versions-more")) {
                    clickTag("versions-more", "")
                    awaitLayoutCalls()
                }
                if (!hasTag(tag)) {
                    val listed = tagsStartingWith("version-").filter { VERSION_TAG.matches(it) }.map { "v" + it.removePrefix("version-") }
                    val err = if (hasTag("versions-error")) "; error shown: ${textUnder("versions-error")}" else ""
                    throw StepFailure("no version ${step.version} in the history; listed: $listed$err")
                }
                val row = onAllNodes(hasTestTag(tag), useUnmergedTree = false).fetchSemanticsNodes().first()
                if (SemanticsProperties.Disabled in row.config) {
                    throw StepFailure("v${step.version} can't be shown by this build: ${textUnder(tag)}")
                }
                clickTag(tag, "")
                awaitLayoutCalls()
                if (hasTag("version-error")) throw StepFailure("error shown: ${textUnder("version-error")}")
                awaitCondition("the menu to close", LAYOUT_TIMEOUT_MS) { !menuShown() }
                val pin = controller.pinned.value
                return (if (pin == null) "following the latest version" else "showing v${pin.version}") to null
            }
            is SimStep.SetServer -> {
                ensureMenuSections()
                requireTag("menu-server", "no Server section in the menu")
                if (!hasTag("server-custom")) clickTag("menu-server", "")
                val key = step.server.lowercase()
                if (key in SERVER_PRESETS) {
                    clickTag("server-$key", "no '$key' server in the menu")
                } else {
                    clickTag("server-custom", "")
                    val field = onAllNodes(hasTestTag("server-custom-url"))
                    if (field.fetchSemanticsNodes().isEmpty()) throw StepFailure("the custom server URL field did not open")
                    field[0].performTextClearance()
                    field[0].performTextInput(step.server)
                    waitForIdle()
                    clickTag("server-custom-use", "")
                }
                waitForIdle()
                return "server: $serverUrl" to null
            }
            is SimStep.PublishVersion -> {
                val publish = publishVersion ?: throw StepFailure("'publish-version' is not supported in $modeName mode")
                val v = publish(step.note)
                waitForIdle()
                return "published v$v" to null
            }
            is SimStep.Screen -> error("handled by run()")
        }
    }

    /** Clicks [node] at its center (scrolled into view first if needed); returns the step message. */
    private fun SkikoComposeUiTest.clickNode(node: SemanticsNodeInteraction, found: String, target: String): String {
        // The click lands at the node's center: it must be on screen, or the event hits nothing (or
        // another node) and the step would "pass" without effect. Scroll it into view if possible.
        var how = found
        if (!centerOnScreen(node)) {
            val scrolled = try {
                node.performScrollTo()
                waitForIdle()
                true
            } catch (e: AssertionError) {
                false
            }
            if (!centerOnScreen(node)) {
                throw StepFailure(
                    "'$target' matched a node ($found) that is not on screen" +
                        (if (scrolled) " even after scrolling to it" else " and has no scrollable parent") +
                        "; bounds ${describeBounds(node)}",
                )
            }
            how = "$found, scrolled into view"
        }
        // The menu button floats over the top-left corner: a node scrolled under it would get the click meant for it.
        if (underMenuButton(node)) {
            node.performSemanticsAction(SemanticsActions.OnClick)
            waitForIdle()
            return "$how, under the menu button: clicked via its click action"
        }
        // A mouse click, then move the pointer off the node: performClick() injects touch
        // events that leave the button in a hovered state, so later shots show a tint.
        node.performMouseInput {
            moveTo(center)
            click(center)
            moveTo(Offset(-1f, -1f))
        }
        waitForIdle()
        // The click may have opened a screen (the menu) whose node under the old pointer position got a
        // hover enter on its first layout; a second exit clears that tint from later shots.
        onRoot().performMouseInput { moveTo(Offset(1f, 1f)); moveTo(Offset(-1f, -1f)) }
        waitForIdle()
        return how
    }

    /** Lets the fake pod finish posts in flight ([settle]), then waits for idle; the step message. */
    private fun SkikoComposeUiTest.settled(): String {
        val n = settle?.invoke() ?: 0
        waitForIdle()
        return if (n > 0) "$n post(s) finished" else ""
    }

    /** Waits until no photo pick or staging is in flight ([MediaCalls]: on a background dispatcher). */
    private fun SkikoComposeUiTest.awaitMediaCalls(timeoutMs: Long = LAYOUT_TIMEOUT_MS) {
        val deadline = System.nanoTime() + timeoutMs * 1_000_000
        while (true) {
            waitForIdle()
            if (MediaCalls.inFlight.value == 0) break
            if (System.nanoTime() > deadline) {
                throw StepFailure("timed out after $timeoutMs ms with ${MediaCalls.inFlight.value} photo pick(s) in flight")
            }
            Thread.sleep(10)
        }
        waitForIdle()
    }

    private fun SkikoComposeUiTest.hasTag(tag: String): Boolean =
        onAllNodes(hasTestTag(tag), useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()

    private fun SkikoComposeUiTest.tagsStartingWith(prefix: String): List<String> =
        onAllNodes(SemanticsMatcher("testTag starts with '$prefix'") {
            it.config.getOrNull(SemanticsProperties.TestTag)?.startsWith(prefix) == true
        }, useUnmergedTree = true).fetchSemanticsNodes().mapNotNull { it.config.getOrNull(SemanticsProperties.TestTag) }

    /** Fails with [hint] unless a node tagged [tag] is on screen (after idle and the catalog calls). */
    private fun SkikoComposeUiTest.requireTag(tag: String, hint: String) {
        awaitLayoutCalls()
        if (!hasTag(tag)) throw StepFailure("$hint; visible text: ${visibleText()}")
    }

    /** Waits up to [timeoutMs] for a node tagged [tag], else fails with [hint]. */
    private fun SkikoComposeUiTest.awaitTag(tag: String, hint: String, timeoutMs: Long = LAYOUT_TIMEOUT_MS) {
        waitForIdle()
        if (hasTag(tag)) return
        try {
            waitUntil("tag $tag", timeoutMs) { hasTag(tag) }
        } catch (e: ComposeTimeoutException) {
            throw StepFailure("$hint (no '$tag' after $timeoutMs ms); visible text: ${visibleText()}")
        }
    }

    private fun SkikoComposeUiTest.clickTag(tag: String, hint: String) {
        waitForIdle()
        val all = onAllNodes(hasTestTag(tag), useUnmergedTree = false)
        if (all.fetchSemanticsNodes().isEmpty()) throw StepFailure("${hint.ifEmpty { "no '$tag' on screen" }}; visible text: ${visibleText()}")
        clickNode(all[0], "test tag", "tag:$tag")
    }

    /** The drawer is showing its sections or the version picker. */
    private fun SkikoComposeUiTest.menuShown(): Boolean = hasTag("menu-drawer") || hasTag("version-picker")

    private fun SkikoComposeUiTest.awaitMenu() {
        waitForIdle()
        try {
            waitUntil("the menu", LAYOUT_TIMEOUT_MS) { menuShown() }
        } catch (e: ComposeTimeoutException) {
            throw StepFailure("the menu did not open; visible text: ${visibleText()}")
        }
    }

    /** Opens the menu if needed and goes back from the version picker to the sections. */
    private fun SkikoComposeUiTest.ensureMenuSections() {
        if (!menuShown()) {
            clickTag("menu-button", "the menu button is not on screen")
            awaitMenu()
        }
        if (hasTag("version-picker")) {
            clickTag("menu-back", "")
            awaitTag("menu-drawer", "the menu did not go back to its sections")
        }
        awaitLayoutCalls()
    }

    private fun SkikoComposeUiTest.appTags(): List<String> =
        tagsStartingWith("app-").filter { !it.startsWith("app-history-") && it != "app-error" }

    /** The tag of the app row for [app] (an id, else a title, case-insensitive), opening the menu as needed. */
    private fun SkikoComposeUiTest.appTag(app: String): String {
        ensureMenuSections()
        val tags = appTags()
        tags.firstOrNull { it == "app-$app" }?.let { return it }
        tags.firstOrNull { it.equals("app-$app", ignoreCase = true) }?.let { return it }
        tags.firstOrNull { textUnder(it).contains(app, ignoreCase = true) }?.let { return it }
        val err = if (hasTag("apps-error")) "; error shown: ${textUnder("apps-error")}" else ""
        throw StepFailure("no app '$app' in the menu; listed: ${tags.map { it.removePrefix("app-") }}$err")
    }

    /** Text of the node tagged [tag] and its descendants. */
    private fun SkikoComposeUiTest.textUnder(tag: String, self: Boolean = false): String {
        val m = if (self) hasTestTag(tag) else (hasTestTag(tag) or hasAnyAncestor(hasTestTag(tag)))
        return onAllNodes(m, useUnmergedTree = true).fetchSemanticsNodes()
            .flatMap { it.config.getOrNull(SemanticsProperties.Text).orEmpty().map { t -> t.text } }
            .distinct().joinToString(" ")
    }

    /**
     * Waits for idle and for every apps call to finish ([LayoutCalls]: they run on Dispatchers.IO, which the test
     * clock does not see), twice in a row, since a finished load can start the next.
     */
    private fun SkikoComposeUiTest.awaitLayoutCalls(timeoutMs: Long = LAYOUT_TIMEOUT_MS) {
        val deadline = System.nanoTime() + timeoutMs * 1_000_000
        var quiet = 0
        while (quiet < 2) {
            waitForIdle()
            if (LayoutCalls.inFlight.value == 0) {
                quiet++
            } else {
                quiet = 0
                if (System.nanoTime() > deadline) {
                    throw StepFailure("timed out after $timeoutMs ms with ${LayoutCalls.inFlight.value} layout call(s) in flight")
                }
                Thread.sleep(10)
            }
        }
    }

    /**
     * Whether a click at [node]'s center would reach it: the center lies inside the node's bounds clipped by
     * its ancestors (a list item scrolled out of its viewport is clipped away) and inside the root.
     */
    private fun SkikoComposeUiTest.centerOnScreen(node: SemanticsNodeInteraction): Boolean {
        val n = node.fetchSemanticsNode()
        val center = n.boundsInRoot.center
        val clipped = n.layoutInfo.coordinates.boundsInRoot()
        val root = onRoot().fetchSemanticsNode().boundsInRoot
        return n.layoutInfo.isAttached && !clipped.isEmpty && clipped.contains(center) && root.contains(center)
    }

    private fun SkikoComposeUiTest.underMenuButton(node: SemanticsNodeInteraction): Boolean {
        val center = node.fetchSemanticsNode().boundsInRoot.center
        val buttons = onAllNodes(hasTestTag("menu-button"), useUnmergedTree = true).fetchSemanticsNodes()
        return buttons.any { it.boundsInRoot.contains(center) } &&
            node.fetchSemanticsNode().config.getOrNull(SemanticsProperties.TestTag) != "menu-button"
    }

    private fun SkikoComposeUiTest.describeBounds(node: SemanticsNodeInteraction): String {
        val n = node.fetchSemanticsNode()
        val root = onRoot().fetchSemanticsNode().boundsInRoot
        return "${n.boundsInRoot}, visible ${n.layoutInfo.coordinates.boundsInRoot()}, screen $root"
    }

    /** Best effort: saves the screen after a failed step as `NN-FAILED.png`; null if that fails too. */
    private fun SkikoComposeUiTest.captureFailure(index: Int): File? = try {
        val file = File(outDir, "%02d-FAILED.png".format(index))
        file.writeBytes(encodePng(onRoot().captureToImage()))
        file
    } catch (e: Throwable) {
        log("[sim] could not capture failure screenshot: ${e::class.simpleName}: ${e.message}")
        null
    }

    private fun SkikoComposeUiTest.awaitCondition(what: String, timeoutMs: Long = textTimeoutMs, condition: () -> Boolean) {
        waitForIdle()
        if (condition()) return
        try {
            waitUntil(what, timeoutMs, condition)
        } catch (e: ComposeTimeoutException) {
            throw StepFailure("timed out after $timeoutMs ms waiting for $what; visible text: ${visibleText()}")
        }
    }

    /** Some node's text or content description contains [text]. */
    private fun SkikoComposeUiTest.hasAnyText(text: String): Boolean =
        nodes(hasText(text, substring = true) or hasContentDescription(text, substring = true)).fetchSemanticsNodes().isNotEmpty()

    /**
     * Waits until no [org.theconstruct.ConstructImage] is loading ([ImageLoads]), then until the last load's
     * crossfade has finished. False on timeout.
     */
    private fun SkikoComposeUiTest.awaitImages(timeoutMs: Long): Boolean {
        waitForIdle()
        val deadline = System.nanoTime() + timeoutMs * 1_000_000
        while (ImageLoads.inFlight.value > 0) {
            if (System.nanoTime() > deadline) return false
            Thread.sleep(10)
            waitForIdle()
        }
        val sinceLast = (System.nanoTime() - SimImages.lastSuccessNanos) / 1_000_000
        if (SimImages.lastSuccessNanos != 0L && sinceLast < CROSSFADE_SETTLE_MS) {
            Thread.sleep(CROSSFADE_SETTLE_MS - sinceLast)
        }
        waitForIdle()
        return true
    }

    private fun SkikoComposeUiTest.nodes(m: SemanticsMatcher): SemanticsNodeInteractionCollection =
        onAllNodes(m, useUnmergedTree = false)

    /**
     * Resolves a step target. `tag:<t>`: the node with test tag `t`, else the first whose tag contains `t`.
     * Otherwise: exact text, then exact content description (clickable nodes first), then the same with substring.
     */
    private fun SkikoComposeUiTest.findTarget(target: String): Pair<SemanticsNodeInteraction, String> {
        val candidates = if (target.startsWith("tag:")) {
            val tag = target.removePrefix("tag:")
            listOf(
                hasTestTag(tag) to "test tag",
                SemanticsMatcher("testTag contains '$tag'") { it.config.getOrNull(SemanticsProperties.TestTag)?.contains(tag) == true } to
                    "test tag substring",
            )
        } else {
            listOf(
                hasText(target) and hasClickAction() to "exact match",
                hasContentDescription(target) and hasClickAction() to "exact content description",
                hasText(target) to "exact match (non-clickable node)",
                hasContentDescription(target) to "exact content description (non-clickable node)",
                hasText(target, substring = true) and hasClickAction() to "substring match",
                hasContentDescription(target, substring = true) and hasClickAction() to "content description substring",
                hasText(target, substring = true) to "substring match (non-clickable node)",
                hasContentDescription(target, substring = true) to "content description substring (non-clickable node)",
            )
        }
        return candidates.firstNotNullOfOrNull { (matcher, how) ->
            val all = nodes(matcher)
            val n = all.fetchSemanticsNodes().size
            if (n == 0) null else all[0] to (if (n > 1) "$how, first of $n" else how)
        } ?: throw StepFailure("no node matches '$target'; visible text: ${visibleText()}")
    }

    private fun SkikoComposeUiTest.visibleText(): String {
        val texts = onAllNodes(SemanticsMatcher("any") { true }, useUnmergedTree = true)
            .fetchSemanticsNodes()
            .flatMap { node ->
                node.config.getOrNull(SemanticsProperties.Text).orEmpty().map { it.text } +
                    node.config.getOrNull(SemanticsProperties.ContentDescription).orEmpty()
            }
            .distinct()
        return texts.joinToString(prefix = "[", postfix = "]") { "\"$it\"" }
    }

    private fun writeLogs(result: SimResult) {
        File(outDir, "steps.log").writeText(
            buildString {
                result.steps.forEach { appendLine(it.logLine()) }
                appendLine(if (result.ok) "RESULT: ok (${result.steps.size} steps)" else "RESULT: FAILED")
            },
        )
        val json = JsonArray(
            result.steps.map { s ->
                JsonObject(
                    linkedMapOf(
                        "index" to JsonPrimitive(s.index),
                        "command" to JsonPrimitive(s.command),
                        "arg" to JsonPrimitive(s.arg),
                        "status" to JsonPrimitive(s.status),
                        "message" to JsonPrimitive(s.message),
                        "file" to (s.file?.name?.let(::JsonPrimitive) ?: JsonNull),
                        "elapsed_ms" to JsonPrimitive(s.elapsedMs),
                    ),
                )
            },
        )
        File(outDir, "steps.json").writeText(
            PRETTY_JSON.encodeToString(JsonArray.serializer(), json) + "\n",
        )
    }

    companion object {
        private val PRETTY_JSON = Json { prettyPrint = true }

        /** How long the menu commands wait for their screen or for the apps calls (live pod calls time out at 15 s). */
        const val LAYOUT_TIMEOUT_MS = 20_000L

        /** How long a `shot` waits for loading images before capturing anyway. */
        const val SHOT_IMAGE_WAIT_MS = 5_000L

        /** Real time allowed for an image's crossfade (Coil's default is 200 ms, driven by the wall clock). */
        const val CROSSFADE_SETTLE_MS = 300L

        /** The menu's Server presets in the simulator (as on Android: staging, the emulator's host, `adb reverse`). */
        val SIM_SERVERS = listOf(
            ServerOption("staging", "Staging", "https://the-construct-pod.fly.dev"),
            ServerOption("emulator", "Local emulator", "http://10.0.2.2:8080"),
            ServerOption("usb", "Local USB", "http://127.0.0.1:8080"),
        )

        private val VERSION_TAG = Regex("version-\\d+")

        fun encodePng(image: ImageBitmap): ByteArray =
            Image.makeFromBitmap(image.asSkiaBitmap()).encodeToData(EncodedImageFormat.PNG)?.bytes
                ?: error("PNG encoding failed")

        fun sanitize(name: String): String =
            name.trim().replace(Regex("[^A-Za-z0-9._-]+"), "_").trim('_').ifEmpty { "shot" }

        private fun ms(startNanos: Long): Long = (System.nanoTime() - startNanos) / 1_000_000
    }
}
