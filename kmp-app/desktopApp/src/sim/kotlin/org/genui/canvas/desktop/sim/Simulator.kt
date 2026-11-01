package org.genui.canvas.desktop.sim

import org.genui.canvas.CanvasController
import org.genui.canvas.desktop.EngineBridge
import uniffi.client_ffi.EngineHandle
import java.io.File
import java.nio.file.Files
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter
import kotlin.system.exitProcess

/**
 * Headless simulator entry point (README "Simulation mode"). Run through
 * `./gradlew :desktopApp:simulate -P<name>=<value>`, which forwards each property as `--<name>=<value>`.
 *
 * Options: workspace (home), screen (phone), script / scriptFile (default "shot initial"),
 * mode (fixture|live), fixture, baseUrl, out, density, timeoutMs (live load wait, 10000).
 * Relative paths resolve against the working directory (the repo root under Gradle).
 * Exit codes: 0 all steps ok, 1 a step failed, 2 bad arguments or script.
 */
fun main(args: Array<String>) {
    val code = try {
        runCli(args)
    } catch (e: UsageException) {
        System.err.println("[sim] error: ${e.message}")
        2
    }
    System.out.flush()
    // The live engine owns non-daemon threads; exit explicitly.
    exitProcess(code)
}

class UsageException(message: String) : Exception(message)

private val KNOWN_OPTIONS = setOf(
    "workspace", "screen", "script", "scriptFile", "mode", "fixture", "baseUrl", "out", "density", "timeoutMs",
)

// Kept reachable for the whole run: the bridge owns the JNA callback observers.
private var liveEngine: EngineHandle? = null
private var liveBridge: EngineBridge? = null
private var liveStorage: File? = null

fun parseArgs(args: Array<String>): Map<String, String> = args.associate { a ->
    if (!a.startsWith("--") || !a.contains('=')) throw UsageException("bad argument '$a' (expected --name=value)")
    val name = a.substring(2).substringBefore('=')
    if (name !in KNOWN_OPTIONS) throw UsageException("unknown option '$name' (known: ${KNOWN_OPTIONS.joinToString()})")
    name to a.substringAfter('=')
}

private fun runCli(args: Array<String>): Int {
    val opts = parseArgs(args).filterValues { it.isNotBlank() }
    val cwd = File("").absoluteFile
    fun path(p: String) = File(p).let { if (it.isAbsolute) it else File(cwd, p) }.normalize()

    val workspace = opts["workspace"] ?: "home"
    val screen = try {
        ScreenSpec.parse(opts["screen"] ?: "phone")
    } catch (e: IllegalArgumentException) {
        throw UsageException(e.message.orEmpty())
    }
    val density = opts["density"]?.let {
        it.toFloatOrNull()?.takeIf { d -> d > 0f && d <= 8f } ?: throw UsageException("bad density '$it' (expected 0 < d <= 8)")
    }
    val timeoutMs = opts["timeoutMs"]?.let { it.toLongOrNull() ?: throw UsageException("bad timeoutMs '$it'") } ?: 10_000L

    val scriptText = when {
        opts["script"] != null && opts["scriptFile"] != null -> throw UsageException("give either script or scriptFile, not both")
        opts["scriptFile"] != null -> path(opts.getValue("scriptFile")).let {
            if (!it.isFile) throw UsageException("script file not found: $it")
            it.readText()
        }
        else -> opts["script"]
    }
    val steps = try {
        parseScript(scriptText ?: "").ifEmpty { listOf(SimStep.Shot("initial")) }
    } catch (e: ScriptParseException) {
        throw UsageException("script: ${e.message}")
    }

    val mode = opts["mode"]?.lowercase() ?: if (opts["baseUrl"] != null) "live" else "fixture"
    if (mode != "fixture" && mode != "live") throw UsageException("bad mode '$mode' (expected fixture or live)")
    if (mode == "live" && opts["fixture"] != null) throw UsageException("fixture cannot be combined with live mode")
    if (mode == "fixture" && opts["baseUrl"] != null) throw UsageException("baseUrl cannot be combined with fixture mode")

    val outDir = opts["out"]?.let(::path) ?: run {
        val root = System.getProperty("canvas.sim.defaultOutRoot")?.let(::File) ?: File(cwd, "kmp-app/desktopApp/build/sim")
        File(root, LocalDateTime.now().format(DateTimeFormatter.ofPattern("yyyyMMdd-HHmmss")))
    }.absoluteFile

    val controller: CanvasController
    var setOnline: ((Boolean) -> Unit)? = null
    if (mode == "fixture") {
        val fixture = opts["fixture"]?.let(::path) ?: defaultFixture(workspace, cwd)
        if (!fixture.isFile) throw UsageException("fixture not found: $fixture")
        println("[sim] mode=fixture workspace=$workspace fixture=$fixture")
        val fake = FakeEngineBridge(fixture.readText())
        setOnline = fake::setOnline
        controller = CanvasController(fake)
    } else {
        val baseUrl = opts["baseUrl"] ?: "http://127.0.0.1:8080"
        // Temp storage so live runs never read or write the app's ~/.canvas offline cache.
        val storage = Files.createTempDirectory("canvas-sim").toFile()
        liveStorage = storage
        println("[sim] mode=live workspace=$workspace baseUrl=$baseUrl storage=$storage")
        val engine = EngineHandle.newWithWorkspace(storage.absolutePath, baseUrl, workspace)
        val bridge = EngineBridge(engine)
        liveEngine = engine
        liveBridge = bridge
        controller = CanvasController(bridge)
        val deadline = System.nanoTime() + timeoutMs * 1_000_000
        while (controller.state.value.workspace == null && System.nanoTime() < deadline) Thread.sleep(50)
        val s = controller.state.value
        if (s.workspace == null) {
            println("[sim] WARNING: workspace '$workspace' did not load within $timeoutMs ms (online=${s.isOnline}); continuing, shots will show Loading/Offline")
        } else {
            println("[sim] workspace loaded (online=${s.isOnline})")
        }
    }

    println("[sim] ${steps.size} step(s), out=$outDir")
    val result = SimRunner(
        controller = controller,
        outDir = outDir,
        screen = screen,
        density = density,
        setOnline = setOnline,
        modeName = mode,
    ).run(steps)
    liveStorage?.deleteRecursively()
    println(if (result.ok) "[sim] OK: ${result.steps.size} step(s)" else "[sim] FAILED (see steps.log)")
    println("[sim] output: ${outDir.absolutePath}")
    return if (result.ok) 0 else 1
}

/** fixtures/ next to (or above) [from]: counter bundle for `home`, Pixelfed bundle for `pixelfed_demo`. */
fun defaultFixture(workspace: String, from: File): File {
    val name = when (workspace) {
        "home" -> "counter_home_bundle.json"
        "pixelfed_demo" -> "pixelfed_home_bundle.json"
        else -> throw UsageException("no default fixture for workspace '$workspace'; pass -Pfixture=<bundle.json> or use live mode")
    }
    return findFixture(name, from) ?: throw UsageException("fixtures/$name not found above $from")
}

fun findFixture(name: String, from: File = File("").absoluteFile): File? =
    generateSequence(from.absoluteFile) { it.parentFile }
        .map { File(it, "fixtures/$name") }
        .firstOrNull { it.isFile }
