package org.genui.canvas.desktop.sim

import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asSkiaBitmap
import androidx.compose.ui.test.ComposeTimeoutException
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.SemanticsNodeInteractionCollection
import androidx.compose.ui.test.SkikoComposeUiTest
import androidx.compose.ui.test.hasClickAction
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.click
import androidx.compose.ui.test.performMouseInput
import androidx.compose.ui.test.runSkikoComposeUiTest
import androidx.compose.ui.unit.Density
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.genui.canvas.CanvasApp
import org.genui.canvas.CanvasController
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
 * Runs [steps] against [CanvasApp] rendered offscreen by the Compose UI test harness
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
    private val controller: CanvasController,
    private val outDir: File,
    private val screen: ScreenSpec,
    private val density: Float? = null,
    private val setOnline: ((Boolean) -> Unit)? = null,
    private val modeName: String = "fixture",
    private val textTimeoutMs: Long = 2_000,
    private val log: (String) -> Unit = ::println,
) {
    fun run(steps: List<SimStep>): SimResult {
        outDir.mkdirs()
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
                    setContent { CanvasApp(controller) }
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
                waitForIdle()
                val image = onRoot().captureToImage()
                val file = File(outDir, "%02d-%s.png".format(index, sanitize(step.name)))
                file.writeBytes(encodePng(image))
                return "${image.width}x${image.height}" to file
            }
            is SimStep.Click -> {
                waitForIdle()
                val (node, how) = findClickTarget(step.text)
                // A mouse click, then move the pointer off the node: performClick() injects touch
                // events that leave the button in a hovered state, so later shots show a tint.
                node.performMouseInput {
                    moveTo(center)
                    click(center)
                    moveTo(Offset(-1f, -1f))
                }
                waitForIdle()
                return how to null
            }
            is SimStep.Wait -> {
                Thread.sleep(step.ms)
                mainClock.advanceTimeBy(step.ms)
                waitForIdle()
                return "" to null
            }
            SimStep.Idle -> {
                waitForIdle()
                return "" to null
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
            is SimStep.Screen -> error("handled by run()")
        }
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

    private fun SkikoComposeUiTest.hasAnyText(text: String): Boolean =
        nodes(hasText(text, substring = true)).fetchSemanticsNodes().isNotEmpty()

    private fun SkikoComposeUiTest.nodes(m: SemanticsMatcher): SemanticsNodeInteractionCollection =
        onAllNodes(m, useUnmergedTree = false)

    /** Exact text on a clickable node, then exact on any node, then the same two with substring. */
    private fun SkikoComposeUiTest.findClickTarget(text: String) =
        listOf(
            hasText(text) and hasClickAction() to "exact match",
            hasText(text) to "exact match (non-clickable node)",
            hasText(text, substring = true) and hasClickAction() to "substring match",
            hasText(text, substring = true) to "substring match (non-clickable node)",
        ).firstNotNullOfOrNull { (matcher, how) ->
            val all = nodes(matcher)
            val n = all.fetchSemanticsNodes().size
            if (n == 0) null else all[0] to (if (n > 1) "$how, first of $n" else how)
        } ?: throw StepFailure("no node with text '$text'; visible text: ${visibleText()}")

    private fun SkikoComposeUiTest.visibleText(): String {
        val texts = onAllNodes(SemanticsMatcher("any") { true }, useUnmergedTree = true)
            .fetchSemanticsNodes()
            .flatMap { node ->
                node.config.getOrNull(SemanticsProperties.Text).orEmpty().map { it.text }
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

        fun encodePng(image: ImageBitmap): ByteArray =
            Image.makeFromBitmap(image.asSkiaBitmap()).encodeToData(EncodedImageFormat.PNG)?.bytes
                ?: error("PNG encoding failed")

        fun sanitize(name: String): String =
            name.trim().replace(Regex("[^A-Za-z0-9._-]+"), "_").trim('_').ifEmpty { "shot" }

        private fun ms(startNanos: Long): Long = (System.nanoTime() - startNanos) / 1_000_000
    }
}
