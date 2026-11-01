package org.genui.canvas.desktop

import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import androidx.compose.ui.window.rememberWindowState
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject
import org.genui.canvas.ActionIntent
import org.genui.canvas.CanvasApp
import org.genui.canvas.CanvasController
import org.genui.canvas.CanvasEngineBridge
import org.genui.canvas.CanvasJson
import java.io.File
import java.net.URI
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.time.Duration
import kotlin.concurrent.thread

fun main() {
    // TEMPORARY: replace FixtureBridge with the UniFFI EngineHandle-backed bridge in the integration step.
    val controller = CanvasController(FixtureBridge())
    application {
        Window(
            onCloseRequest = ::exitApplication,
            title = "Project Canvas",
            state = rememberWindowState(width = 480.dp, height = 720.dp),
        ) {
            CanvasApp(controller)
        }
    }
}

/**
 * TEMPORARY stand-in for the Rust engine, to be REPLACED by the UniFFI `EngineHandle` bridge
 * in the integration step. Loads the bundle from the local server
 * (http://127.0.0.1:8080/v1/workspaces/home), falling back to fixtures/counter_home_bundle.json
 * (override with -Dcanvas.fixture=<path>). Mimics the engine snapshot contract: the workspace
 * layout node JSON (or "null" before load), a URI-keyed entities JSON object, and isOnline.
 * Applies OptimisticMutate on core://counters/home locally (no network writes).
 */
private class FixtureBridge : CanvasEngineBridge {
    private val lock = Any()
    private var listener: ((String, String, Boolean) -> Unit)? = null
    private var workspaceJson = "null"
    private var entities: MutableMap<String, JsonObject> = mutableMapOf()
    private var online = false

    init {
        // Load off the UI thread; callbacks arrive on a background thread like the real engine's.
        thread(name = "fixture-bridge-load", isDaemon = true) { load() }
    }

    override fun subscribe(onState: (workspaceJson: String, entitiesJson: String, isOnline: Boolean) -> Unit) {
        synchronized(lock) { listener = onState }
        notifyListener()
    }

    override fun dispatch(intentJson: String) {
        val intent = runCatching { CanvasJson.decodeIntent(intentJson) }.getOrNull()
        if (intent !is ActionIntent.OptimisticMutate || intent.entityUri != COUNTER_URI) {
            println("[FixtureBridge] ignoring intent: $intentJson")
            return
        }
        synchronized(lock) {
            val entity = entities[COUNTER_URI] ?: JsonObject(emptyMap())
            val current = (entity[intent.field] as? JsonPrimitive)?.intOrNull ?: 0
            val next = if (intent.valueExpr.contains("+ 1")) current + 1 else intent.valueExpr.trim().toIntOrNull() ?: 0
            entities[COUNTER_URI] = JsonObject(entity + (intent.field to JsonPrimitive(next)))
        }
        notifyListener()
    }

    private fun load() {
        val (body, fromServer) = fetchFromServer()?.let { it to true } ?: (readFixture() to false)
        if (body == null) {
            println("[FixtureBridge] no server and no fixture found; staying in loading state")
            return
        }
        val root = Json.parseToJsonElement(body).jsonObject
        synchronized(lock) {
            workspaceJson = root["workspace"].toString()
            entities = root["entities"]?.jsonObject?.mapValues { it.value.jsonObject }?.toMutableMap() ?: mutableMapOf()
            online = fromServer
        }
        println("[FixtureBridge] loaded workspace from ${if (fromServer) "server" else "fixture"}")
        notifyListener()
    }

    private fun fetchFromServer(): String? = try {
        val client = HttpClient.newBuilder().connectTimeout(Duration.ofSeconds(2)).build()
        val req = HttpRequest.newBuilder(URI.create(SERVER_URL)).timeout(Duration.ofSeconds(3)).GET().build()
        val resp = client.send(req, HttpResponse.BodyHandlers.ofString())
        if (resp.statusCode() == 200) resp.body() else null.also { println("[FixtureBridge] server returned ${resp.statusCode()}") }
    } catch (e: Exception) {
        println("[FixtureBridge] server unavailable ($e); using fixture")
        null
    }

    private fun readFixture(): String? {
        val candidates = listOfNotNull(
            System.getProperty("canvas.fixture"),
            "../../fixtures/counter_home_bundle.json", // cwd = kmp-app/desktopApp (gradle run)
            "../fixtures/counter_home_bundle.json", // cwd = kmp-app
            "fixtures/counter_home_bundle.json", // cwd = repo root
        )
        return candidates.map(::File).firstOrNull { it.isFile }?.readText()
    }

    private fun notifyListener() {
        val (l, ws, ents, on) = synchronized(lock) {
            Snapshot(listener, workspaceJson, JsonObject(entities.toMap()).toString(), online)
        }
        l?.invoke(ws, ents, on)
    }

    private data class Snapshot(val l: ((String, String, Boolean) -> Unit)?, val ws: String, val ents: String, val on: Boolean)

    companion object {
        const val SERVER_URL = "http://127.0.0.1:8080/v1/workspaces/home"
        const val COUNTER_URI = "core://counters/home"
    }
}
