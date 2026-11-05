package org.theconstruct.desktop.sim

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import org.theconstruct.ConstructController
import org.theconstruct.ConstructJson
import org.theconstruct.engine.UniffiEngineBridge
import java.io.File
import java.nio.file.Files
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter
import kotlin.system.exitProcess

/**
 * Headless simulator entry point (docs/simulator.md). Run through
 * `./gradlew :desktopApp:simulate -P<name>=<value>`, which forwards each property as `--<name>=<value>`.
 *
 * Options: workspace (pixelfed), screen (phone), script / scriptFile (default "shot initial"),
 * mode (fixture|live), fixture, baseUrl, out, density, timeoutMs (live load wait, 10000),
 * today (`YYYY-MM-DD`: the day dates are drawn against, `core://Text` `format`; default the real date),
 * storage (live only: persistent engine storage dir; default a fresh temp dir deleted on exit),
 * pixelfedLogin (live only: `<email or seed username>:<password>` signs in to a real Pixelfed's web UI and
 * approves the consent page, see [PixelfedLoginBrowser]; default: [AutoApproveBrowser] for the fake upstream).
 * Relative paths resolve against the working directory (the repo root under Gradle).
 * Exit codes: 0 all steps ok, 1 a step failed, 2 bad arguments or script.
 */
fun main(args: Array<String>) {
    val code = try {
        runCli(args)
    } catch (e: UsageException) {
        System.err.println("[sim] error: ${e.message}")
        2
    } catch (e: Throwable) {
        // Must still reach exitProcess: an uncaught exception would leave the live engine's
        // non-daemon threads running and the JVM (and Gradle) hanging.
        System.err.println("[sim] internal error: $e")
        e.printStackTrace()
        1
    } finally {
        liveStorage?.deleteRecursively()
    }
    System.out.flush()
    // The live engine owns non-daemon threads; exit explicitly.
    exitProcess(code)
}

class UsageException(message: String) : Exception(message)

/** The app the simulator shows unless -Pworkspace says otherwise (as on every surface: the one Pixelfed app). */
const val DEFAULT_WORKSPACE = "pixelfed"

/** Live mode's server unless -PbaseUrl is given: the staging pod, as in the desktop app. */
const val LIVE_DEFAULT_BASE_URL = "https://the-construct-pod.fly.dev"

private val KNOWN_OPTIONS = setOf(
    "workspace", "screen", "script", "scriptFile", "mode", "fixture", "baseUrl", "out", "density", "timeoutMs",
    "storage", "pixelfedLogin", "today",
)

// Kept reachable for the whole run: the bridge owns the engine handle and the JNA callback observers.
private var liveBridge: UniffiEngineBridge? = null
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

    val workspace = opts["workspace"] ?: DEFAULT_WORKSPACE
    val screen = try {
        ScreenSpec.parse(opts["screen"] ?: "phone")
    } catch (e: IllegalArgumentException) {
        throw UsageException(e.message.orEmpty())
    }
    val density = opts["density"]?.let {
        it.toFloatOrNull()?.takeIf { d -> d > 0f && d <= 8f } ?: throw UsageException("bad density '$it' (expected 0 < d <= 8)")
    }
    val today = opts["today"]?.let {
        org.theconstruct.isoDateEpochDay(it) ?: throw UsageException("bad today '$it' (expected YYYY-MM-DD)")
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
    if (mode == "fixture" && opts["storage"] != null) throw UsageException("storage cannot be combined with fixture mode")
    if (mode == "fixture" && opts["pixelfedLogin"] != null) throw UsageException("pixelfedLogin cannot be combined with fixture mode")
    val pixelfedLogin = opts["pixelfedLogin"]?.let {
        val user = it.substringBefore(':')
        val pass = it.substringAfter(':', "")
        if (user.isBlank() || pass.isEmpty()) throw UsageException("pixelfedLogin must be <email or username>:<password>")
        user to pass
    }
    try {
        validateStepsForMode(steps, mode)
    } catch (e: ScriptParseException) {
        throw UsageException("script: ${e.message}")
    }

    val outDir = opts["out"]?.let(::path) ?: run {
        val root = System.getProperty("construct.sim.defaultOutRoot")?.let(::File) ?: File(cwd, "app/desktopApp/build/sim")
        File(root, LocalDateTime.now().format(DateTimeFormatter.ofPattern("yyyyMMdd-HHmmss")))
    }.absoluteFile
    requireWritableDir(outDir)

    val controller: ConstructController
    var setOnline: ((Boolean) -> Unit)? = null
    var publishVersion: ((String) -> Long)? = null
    var notify: ((String, String, String) -> Unit)? = null
    var settle: (() -> Int)? = null
    if (mode == "fixture") {
        val fixture = opts["fixture"]?.let(::path) ?: defaultFixture(workspace, cwd)
        if (!fixture.isFile) throw UsageException("fixture not found: $fixture")
        println("[sim] mode=fixture workspace=$workspace fixture=$fixture")
        // A custom fixture without -Pworkspace belongs to the app its bundle names.
        val appId = if (opts["fixture"] != null && opts["workspace"] == null) null else workspace
        // The fake pod's data path answers (screens, D34), generated from the pod: Pixelfed's comments by
        // crates/server/tests/fixtures.rs, the Events app's screens by crates/app-events/tests/events.rs.
        val screenData = listOf("pixelfed_screen_data.json", "events_screen_data.json")
            .mapNotNull { findFixture(it, cwd)?.readText() }
            .map { ConstructJson.json.parseToJsonElement(it).jsonObject }
            .takeIf { it.isNotEmpty() }
            ?.let { parts -> JsonObject(parts.fold(emptyMap<String, JsonElement>()) { acc, o -> acc + o }).toString() }
        // The fake pod's notifications (D52), shared by every engine of the run.
        val notifications = findFixture("notifications.json", cwd)?.let { FakeNotifications.fromFixture(it.readText()) }
            ?: FakeNotifications()
        // The device's remembered form values (schema 10), kept across open-app like the engine's cache.db.
        val formValues = FakeFormValues()
        var current = try {
            FakeEngineBridge(
                fixture.readText(), appId = appId, screenData = screenData, notifications = notifications,
                formValues = formValues, layoutBundles = { id -> layoutBundle(id, cwd) },
            )
        } catch (e: Exception) {
            throw UsageException("bad fixture $fixture: ${e.message}")
        }
        setOnline = { current.setOnline(it) }
        publishVersion = { current.publishVersion(it) }
        notify = { app, channel, title -> notifications.inject(app, channel, title) }
        settle = { current.settle() }
        // One engine for every layout on screen (decision `layouts-not-apps`).
        controller = ConstructController(current, current.appId)
    } else {
        // Same default as the desktop app (the staging pod); pass -PbaseUrl=http://127.0.0.1:8080 for a local server.
        val baseUrl = opts["baseUrl"] ?: LIVE_DEFAULT_BASE_URL
        // By default temp storage (deleted on exit) so live runs never read or write the app's ~/.construct
        // cache; -Pstorage=<dir> keeps a persistent dir (restart-persistence QA, inspecting cache.db).
        val storage = opts["storage"]?.let { dir ->
            path(dir).absoluteFile.also { requireWritableDir(it, "storage directory") }
        } ?: Files.createTempDirectory("construct-sim").toFile().also { liveStorage = it }
        println("[sim] mode=live workspace=$workspace baseUrl=$baseUrl storage=$storage")
        // Sign-in: in-memory session store and a browser that auto-approves (works with the fake upstream), or,
        // with pixelfedLogin, one that logs in to a real Pixelfed and approves; loopback redirect. A script signs
        // in with e.g. `click Sign in with Pixelfed; wait-text 20000 @constructdemo`.
        val opener = pixelfedLogin?.let { (user, pass) ->
            println("[sim] sign-in: scripted Pixelfed web login as $user")
            PixelfedLoginBrowser(user, pass)
        } ?: AutoApproveBrowser()
        // One session store and browser for every engine of the run, so a sign-in carries over to apps opened later.
        val secureStore = InMemorySecureStore()
        fun open(appId: String): UniffiEngineBridge = UniffiEngineBridge.create(
            storage.absolutePath, baseUrl, appId,
            secureStore = secureStore,
            urlOpener = opener,
        ).also { liveBridge = it }
        // One engine for every layout on screen (decision `layouts-not-apps`).
        controller = ConstructController(open(workspace), workspace)
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
        today = today,
        setOnline = setOnline,
        modeName = mode,
        publishVersion = publishVersion,
        notify = notify,
        settle = settle,
        server = if (mode == "live") opts["baseUrl"] ?: LIVE_DEFAULT_BASE_URL else SimRunner.SIM_SERVERS.first().url,
    ).run(steps)
    if (result.ok) {
        println("[sim] OK: ${result.steps.size} step(s)")
    } else {
        val failed = result.steps.last { !it.ok }
        System.err.println("[sim] FAILED at step ${failed.index} (${failed.command}): ${failed.message}")
        failed.file?.let { System.err.println("[sim] failure screenshot: ${it.absolutePath}") }
        System.err.println("[sim] see ${File(outDir, "steps.log").absolutePath}")
    }
    println("[sim] output: ${outDir.absolutePath}")
    return if (result.ok) 0 else 1
}

/** Creates [dir] if needed; a usage error unless it ends up a writable directory. */
fun requireWritableDir(dir: File, what: String = "output directory") {
    try {
        Files.createDirectories(dir.toPath())
    } catch (e: Exception) {
        throw UsageException("cannot create $what $dir: ${e::class.simpleName}: ${e.message}")
    }
    if (!dir.isDirectory || !Files.isWritable(dir.toPath())) throw UsageException("$what $dir is not a writable directory")
}

/**
 * The default bundle for [workspace] ([findFixture]): the one-post Pixelfed sample for `pixelfed_sample_no_login`, the
 * normalized fake-upstream timeline for `pixelfed`.
 */
fun defaultFixture(workspace: String, from: File): File {
    val name = when (workspace) {
        "pixelfed_sample_no_login" -> "pixelfed_sample_bundle.json"
        "pixelfed" -> "pixelfed_feed_bundle.json"
        FakeAppCatalog.KITCHEN -> "custom_kitchen_bundle.json"
        "events" -> "events_bundle.json"
        else -> throw UsageException("no default fixture for workspace '$workspace'; pass -Pfixture=<bundle.json> or use live mode")
    }
    return findFixture(name, from) ?: throw UsageException("${fixturePath(name)} not found above $from")
}

/** The simulator's own fixture files (test and simulator data only; nothing here ships). */
const val SIM_FIXTURES = "app/desktopApp/src/sim/resources"

/**
 * Where fixture [name] lives, relative to the repo root: in [SIM_FIXTURES], except two that belong to other code
 * and are shared from there: the sample app's data (shipped in the pod) and the protocol tests' counter bundle.
 */
fun fixturePath(name: String): String = when (name) {
    "pixelfed_sample_bundle.json" -> "crates/app-pixelfed/sample/bundle.json"
    "counter_home_bundle.json" -> "crates/protocol/tests/data/counter_home_bundle.json"
    else -> "$SIM_FIXTURES/$name"
}

/**
 * The fake pod's bundle for layout [id] (fixture mode, decision `layouts-not-apps`): a blueprint of `app-shell` as a pod
 * serves it to this build (`blueprint_<id>_page.json`, plumbing: decision `plumbing-and-porcelain`; with no data:
 * Settings and the inbox read only device data), else the layout's default fixture; null when there is none.
 * `UPDATE_FIXTURES=1 cargo test -p server --test integration fixtures::` writes the blueprint files.
 */
fun layoutBundle(id: String, from: File): String? {
    val blueprint = findFixture("blueprint_${id}_page.json", from)
    if (blueprint != null) {
        return """{"version": "1.0.0", "workspace": ${blueprint.readText()}, "entities": {}, "queries": {}}"""
    }
    return try {
        defaultFixture(id, from).readText()
    } catch (e: UsageException) {
        null
    }
}

/** Fixture [name] ([fixturePath]) in the repo at or above [from]. */
fun findFixture(name: String, from: File = File("").absoluteFile): File? =
    generateSequence(from.absoluteFile) { it.parentFile }
        .map { File(it, fixturePath(name)) }
        .firstOrNull { it.isFile }

/** The fake Pixelfed's images (`fake-pixelfed/fixtures/media`), the ones its fixture posts link to. */
fun findPixelfedMedia(from: File = File("").absoluteFile): File? =
    generateSequence(from.absoluteFile) { it.parentFile }
        .map { File(it, "fake-pixelfed/fixtures/media") }
        .firstOrNull { it.isDirectory }
