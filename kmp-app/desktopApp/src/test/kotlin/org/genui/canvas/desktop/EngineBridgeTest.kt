package org.genui.canvas.desktop

import com.sun.net.httpserver.HttpServer
import org.genui.canvas.CanvasJson
import org.genui.canvas.engine.UniffiEngineBridge
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
import kotlin.test.assertTrue
import kotlin.test.fail

/**
 * End-to-end over JNA: Kotlin [UniffiEngineBridge] (:shared jvmCommonMain) -> UniFFI -> Rust CanvasEngine -> StateObserver
 * callback. A stub HTTP server serves fixtures/counter_home_bundle.json.
 * Requires target/release/libclient_ffi.dylib (built by :shared:buildRustFfi).
 */
class EngineBridgeTest {
    private lateinit var server: HttpServer
    private lateinit var storage: File

    @BeforeTest
    fun setUp() {
        val bundle = generateSequence(File("").absoluteFile) { it.parentFile }
            .map { File(it, "fixtures/counter_home_bundle.json") }
            .first { it.isFile }
            .readBytes()
        server = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0).apply {
            createContext("/v1/workspaces/home") { ex ->
                ex.responseHeaders.add("Content-Type", "application/json")
                ex.sendResponseHeaders(200, bundle.size.toLong())
                ex.responseBody.use { it.write(bundle) }
            }
            start()
        }
        storage = Files.createTempDirectory("canvas-engine-test").toFile()
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
        bridge.subscribe { _, entitiesJson, _ ->
            val e = entitiesJson.trim()
            if (e.isNotEmpty() && e != "null") {
                CanvasJson.decodeEntities(e)["core://counters/home"]?.get("count")
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
    }

    private fun awaitCount(counts: LinkedBlockingQueue<Int>, expected: Int) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(10)
        while (System.nanoTime() < deadline) {
            if (counts.poll(200, TimeUnit.MILLISECONDS) == expected) return
        }
        fail("counter never reached $expected")
    }

    private companion object {
        const val INCREMENT = """{"intent":"action.mutation.optimistic","entity_uri":"core://counters/home","field":"count","value_expr":"entity.count + 1","rollback_value":0,"endpoint":"/v1/counters/home/increment","payload":{}}"""
    }
}
