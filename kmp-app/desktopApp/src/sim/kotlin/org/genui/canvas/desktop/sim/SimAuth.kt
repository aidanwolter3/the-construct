package org.genui.canvas.desktop.sim

import uniffi.client_ffi.SecureStore
import uniffi.client_ffi.UrlOpener
import java.net.URI
import java.net.URLDecoder
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.time.Duration
import java.util.concurrent.ConcurrentHashMap

/** Live-mode session store: in memory, so simulator runs never touch the Keychain (or each other). */
class InMemorySecureStore : SecureStore {
    private val map = ConcurrentHashMap<String, String>()
    override fun get(key: String): String? = map[key]
    override fun set(key: String, value: String) {
        map[key] = value
    }
    override fun delete(key: String) {
        map.remove(key)
    }
}

/**
 * Live-mode "browser" that approves sign-in by itself (DECISIONS D19): GETs the authorize URL with `auto=1`
 * (the fake upstream then answers 302 straight to the redirect URI, without a login page). If that redirect
 * points at the engine's `redirect_uri` (the loopback listener), it is followed on a background thread (the
 * engine only starts serving the listener after `open` returns) with redirects followed
 * (`HttpClient.Redirect.NORMAL`). Anything else (a real Pixelfed sends a login page or a redirect to /login,
 * which the simulator cannot fill in) prints the URL for a human and returns false, so the engine reports
 * "no browser available" and keeps waiting.
 */
class AutoApproveBrowser(private val log: (String) -> Unit = { println("[sim] $it") }) : UrlOpener {
    private val noRedirect = HttpClient.newBuilder()
        .followRedirects(HttpClient.Redirect.NEVER)
        .connectTimeout(Duration.ofSeconds(5))
        .build()
    private val following = HttpClient.newBuilder()
        .followRedirects(HttpClient.Redirect.NORMAL)
        .connectTimeout(Duration.ofSeconds(5))
        .build()

    override fun open(url: String): Boolean {
        val approveUrl = url + (if ('?' in url) "&" else "?") + "auto=1"
        val redirectUri = queryParam(url, "redirect_uri")
        return try {
            val resp = noRedirect.send(get(approveUrl), HttpResponse.BodyHandlers.discarding())
            val location = resp.headers().firstValue("location").orElse(null)
            if (resp.statusCode() in 300..399 && location != null && redirectUri != null && location.startsWith(redirectUri)) {
                log("browser: authorize auto-approved (HTTP ${resp.statusCode()}); following the redirect to $redirectUri")
                Thread({
                    try {
                        val r = following.send(get(location), HttpResponse.BodyHandlers.ofString())
                        log("browser: redirect target answered HTTP ${r.statusCode()}")
                    } catch (e: Exception) {
                        log("browser: following the redirect failed: $e")
                    }
                }, "sim-browser").apply { isDaemon = true }.start()
                true
            } else {
                log(
                    "browser: the authorize page did not auto-approve (HTTP ${resp.statusCode()}" +
                        (location?.let { " -> $it" } ?: "") + "); a real instance needs a human. Open: $url",
                )
                false
            }
        } catch (e: Exception) {
            log("browser: cannot load the authorize URL ($e). Open: $url")
            false
        }
    }

    private fun get(url: String) = HttpRequest.newBuilder(URI(url)).timeout(Duration.ofSeconds(10)).GET().build()

    companion object {
        fun queryParam(url: String, name: String): String? =
            URI(url).rawQuery?.split('&')?.firstNotNullOfOrNull { kv ->
                val (k, v) = kv.split('=', limit = 2).let { it[0] to it.getOrElse(1) { "" } }
                if (URLDecoder.decode(k, Charsets.UTF_8) == name) URLDecoder.decode(v, Charsets.UTF_8) else null
            }
    }
}
