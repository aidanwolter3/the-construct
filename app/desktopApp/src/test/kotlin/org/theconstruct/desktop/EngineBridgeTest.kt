package org.theconstruct.desktop

import com.sun.net.httpserver.HttpServer
import org.theconstruct.ConstructJson
import org.theconstruct.LayoutApiException
import org.theconstruct.engine.UniffiEngineBridge
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.PrintStream
import java.net.InetSocketAddress
import java.nio.file.Files
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import kotlin.test.AfterTest
import kotlin.test.BeforeTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import kotlin.test.fail

/**
 * End-to-end over JNA: Kotlin [UniffiEngineBridge] (:shared jvmCommonMain) -> UniFFI -> Rust ConstructEngine -> StateObserver
 * callback. A stub HTTP server serves crates/protocol/tests/data/counter_home_bundle.json and a small `/v1/apps` catalog.
 * Requires target/release/libclient_ffi.dylib (built by :shared:buildRustFfi).
 */
class EngineBridgeTest {
    private lateinit var server: HttpServer
    private lateinit var storage: File
    private val requests = mutableListOf<String>()

    @BeforeTest
    fun setUp() {
        val bundle = generateSequence(File("").absoluteFile) { it.parentFile }
            .map { File(it, "crates/protocol/tests/data/counter_home_bundle.json") }
            .first { it.isFile }
            .readBytes()
        server = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0).apply {
            createContext("/v1/workspaces/home") { ex ->
                ex.responseHeaders.add("Content-Type", "application/json")
                ex.sendResponseHeaders(200, bundle.size.toLong())
                ex.responseBody.use { it.write(bundle) }
            }
            createContext("/v1/apps") { ex ->
                val path = ex.requestURI.path
                val query = ex.requestURI.rawQuery
                synchronized(requests) { requests += if (query == null) path else "$path?$query" }
                val (status, body) = when (path) {
                    "/v1/apps" -> 200 to APPS
                    "/v1/apps/home/versions" -> 200 to VERSIONS
                    "/v1/apps/home/versions/1" -> 200 to VERSION_1
                    "/v1/apps/home/versions/9" -> 404 to """{"error":"version_not_found"}"""
                    else -> 404 to """{"error":"app_not_found"}"""
                }
                val bytes = body.toByteArray()
                ex.responseHeaders.add("Content-Type", "application/json")
                ex.sendResponseHeaders(status, bytes.size.toLong())
                ex.responseBody.use { it.write(bytes) }
            }
            start()
        }
        storage = Files.createTempDirectory("construct-engine-test").toFile()
    }

    @AfterTest
    fun tearDown() {
        server.stop(0)
        storage.deleteRecursively()
    }

    @Test
    fun dispatchIncrementUpdatesCounterThroughJna() {
        val bridge = UniffiEngineBridge.create(
            storage.absolutePath, "http://127.0.0.1:${server.address.port}", "home",
        )
        val counts = LinkedBlockingQueue<Int>()
        val seqs = mutableListOf<Long>()
        bridge.subscribe { snapshot ->
            synchronized(seqs) { seqs += snapshot.seq }
            val e = snapshot.entitiesJson.trim()
            if (e.isNotEmpty() && e != "null") {
                ConstructJson.decodeEntities(e)["core://counters/home"]?.get("count")
                    ?.toString()?.toIntOrNull()?.let(counts::add)
            }
        }
        awaitCount(counts, 0)

        val out = ByteArrayOutputStream()
        val oldOut = System.out
        System.setOut(PrintStream(out, true))
        try {
            bridge.dispatch(INCREMENT)
        } finally {
            System.setOut(oldOut)
        }
        assertTrue(out.toString().contains("[desktop] dispatch: $INCREMENT"), "missing dispatch log: $out")
        awaitCount(counts, 1)
        // seq is carried through the FFI and grows across snapshots (callbacks may interleave, so not
        // necessarily monotonic in arrival order).
        synchronized(seqs) { assertTrue(seqs.size >= 2 && seqs.max() > seqs.min(), "seqs: $seqs") }
    }

    @Test
    fun appCatalogCallsPassJsonThroughAndMapErrors() {
        val bridge = UniffiEngineBridge.create(
            storage.absolutePath, "http://127.0.0.1:${server.address.port}", "home",
        )
        try {
            assertEquals(APPS, bridge.listApps())
            assertEquals(VERSIONS, bridge.appVersions("home", before = 3L, limit = 2))
            assertEquals(VERSIONS, bridge.appVersions("home"))
            assertEquals(VERSION_1, bridge.appVersion("home", 1L))
            synchronized(requests) {
                assertEquals(
                    listOf("/v1/apps?all=1", "/v1/apps/home/versions?before=3&limit=2", "/v1/apps/home/versions", "/v1/apps/home/versions/1"),
                    requests,
                )
            }

            val missingVersion = assertFailsWith<LayoutApiException> { bridge.appVersion("home", 9L) }
            assertEquals("That version doesn't exist.", missingVersion.message)
            assertEquals(LayoutApiException.Kind.NotFound, missingVersion.kind)
            val missingApp = assertFailsWith<LayoutApiException> { bridge.appVersions("nope") }
            assertEquals("That app doesn't exist.", missingApp.message)
        } finally {
            bridge.close()
        }
    }

    @Test
    fun unreachablePodIsOfflineAndClosedBridgeIsInert() {
        val bridge = UniffiEngineBridge.create(storage.absolutePath, "http://127.0.0.1:1", "home")
        val offline = assertFailsWith<LayoutApiException> { bridge.listApps() }
        assertEquals(LayoutApiException.Kind.Offline, offline.kind)
        assertEquals("Can't reach your pod. Check your connection and try again.", offline.message)

        bridge.close()
        bridge.close() // idempotent
        bridge.dispatch(INCREMENT) // ignored, no crash
        bridge.logout()
        assertFalse(bridge.handleAuthRedirect("construct://oauth?code=x&state=y"))
        val closed = assertFailsWith<LayoutApiException> { bridge.appVersion("home", 1L) }
        assertEquals(LayoutApiException.Kind.Offline, closed.kind)
    }

    private fun awaitCount(counts: LinkedBlockingQueue<Int>, expected: Int) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(10)
        while (System.nanoTime() < deadline) {
            if (counts.poll(200, TimeUnit.MILLISECONDS) == expected) return
        }
        fail("counter never reached $expected")
    }

    private companion object {
        const val APPS = """{"apps":[{"id":"home","title":"Counter","icon":"counter","latest_version":2,"updated_at":"2026-11-02T16:32:28Z"}]}"""
        const val VERSIONS = """{"app_id":"home","latest_version":2,"versions":[{"version":2,"created_at":"2026-11-02T16:32:28Z","source":"file","note":"Counter"}],"next_before":2}"""
        const val VERSION_1 = """{"app_id":"home","version":1,"created_at":"2026-11-02T11:33:34Z","source":"seed","layout":{"type":"core://Text","value_expr":"x","style":"Body"}}"""
        const val INCREMENT = """{"intent":"action.mutation.optimistic","entity_uri":"core://counters/home","field":"count","value_expr":"entity.count + 1","rollback_value":0,"endpoint":"/v1/counters/home/increment","payload":{}}"""
    }
}
