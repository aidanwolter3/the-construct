package org.theconstruct.desktop.sim

import uniffi.client_ffi.SecureStore
import uniffi.client_ffi.UrlOpener
import java.net.CookieManager
import java.net.CookiePolicy
import java.net.URI
import java.net.URLDecoder
import java.net.URLEncoder
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

/**
 * Live-mode "browser" for a REAL Pixelfed instance (README "Local Pixelfed instance"; DECISIONS D21), opt-in
 * with `-PpixelfedLogin=<user>:<password>`. Pixelfed's authorize page needs a logged-in web session, so this
 * scripts what a person would do, with its own cookie jar and no redirect following:
 * 1. `GET <origin>/login`, read the CSRF `_token` (the session cookie is set);
 * 2. `POST /login` with `_token`, `email`, `password` (Pixelfed logs in by e-mail only; a [login] without `@` is
 *    taken as a seed username and sent as `<login>@example.test`, the domain `scripts/pixelfed_dev.sh seed` uses);
 *    a 302 back to `/login` means the credentials were rejected;
 * 3. `GET` the authorize URL: an already-approved client 302s straight to the redirect URI; otherwise the consent
 *    page has two forms posting to `/oauth/authorize`: approve (`_token`, `state`, `client_id`, `auth_token`) and
 *    deny (the same plus `_method=DELETE`);
 * 4. `POST` the approve form's hidden fields; the 302 `Location` (redirect URI with `code` and `state`) is followed on
 *    a daemon thread, because the engine serves the loopback listener only after `open` returns.
 * Any failure logs the reason and the URL and returns false (the engine then reports "no browser available").
 * `/login` and the `/oauth/` routes answer only on the instance's `APP_DOMAIN` host (`localhost` for the dev stack) and
 * `/oauth/authorize` is throttled to 10 requests a minute.
 */
class PixelfedLoginBrowser(
    login: String,
    private val password: String,
    private val log: (String) -> Unit = { println("[sim] $it") },
) : UrlOpener {
    private val email = if ('@' in login) login else "$login@example.test"
    private val client = HttpClient.newBuilder()
        .followRedirects(HttpClient.Redirect.NEVER)
        .cookieHandler(CookieManager(null, CookiePolicy.ACCEPT_ALL))
        .connectTimeout(Duration.ofSeconds(5))
        .build()
    private val following = HttpClient.newBuilder()
        .followRedirects(HttpClient.Redirect.NORMAL)
        .connectTimeout(Duration.ofSeconds(5))
        .build()

    override fun open(url: String): Boolean = try {
        signIn(url)
    } catch (e: Exception) {
        log("pixelfed login: failed ($e). Open: $url")
        false
    }

    private fun signIn(url: String): Boolean {
        val redirectUri = AutoApproveBrowser.queryParam(url, "redirect_uri")
            ?: return fail("authorize URL has no redirect_uri", url)
        val origin = URI(url).let { "${it.scheme}://${it.rawAuthority}" }

        val loginPage = client.send(get("$origin/login"), HttpResponse.BodyHandlers.ofString())
        if (loginPage.statusCode() != 200) return fail("GET /login answered HTTP ${loginPage.statusCode()}", url)
        val csrf = hiddenInputs(loginPage.body())["_token"] ?: return fail("no _token on the login page", url)
        val loggedIn = client.send(
            postForm("$origin/login", listOf("_token" to csrf, "email" to email, "password" to password)),
            HttpResponse.BodyHandlers.discarding(),
        )
        val afterLogin = loggedIn.headers().firstValue("location").orElse("")
        if (loggedIn.statusCode() !in 300..399 || afterLogin.substringBefore('?').endsWith("/login")) {
            return fail("login as $email rejected (HTTP ${loggedIn.statusCode()}${if (afterLogin.isEmpty()) "" else " -> $afterLogin"})", url)
        }
        log("pixelfed login: signed in to the web UI as $email")

        val page = client.send(get(url), HttpResponse.BodyHandlers.ofString())
        val location = when (page.statusCode()) {
            in 300..399 -> page.headers().firstValue("location").orElse("").also {
                log("pixelfed login: client already approved; authorize redirected")
            }
            200 -> {
                val form = approveForm(page.body()) ?: return fail("no approve form on the authorize page", url)
                val approved = client.send(postForm(form.first, form.second), HttpResponse.BodyHandlers.discarding())
                if (approved.statusCode() !in 300..399) return fail("approve POST answered HTTP ${approved.statusCode()}", url)
                log("pixelfed login: consent approved")
                approved.headers().firstValue("location").orElse("")
            }
            else -> return fail("authorize page answered HTTP ${page.statusCode()}", url)
        }
        if (!location.startsWith(redirectUri)) return fail("authorize redirected to '$location', not $redirectUri", url)
        Thread({
            try {
                val r = following.send(get(location), HttpResponse.BodyHandlers.ofString())
                log("pixelfed login: redirect target answered HTTP ${r.statusCode()}")
            } catch (e: Exception) {
                log("pixelfed login: following the redirect failed: $e")
            }
        }, "sim-pixelfed-login").apply { isDaemon = true }.start()
        return true
    }

    private fun fail(why: String, url: String): Boolean {
        log("pixelfed login: $why. Open: $url")
        return false
    }

    private fun get(url: String) = HttpRequest.newBuilder(URI(url))
        .timeout(Duration.ofSeconds(15))
        .header("Accept", "text/html")
        .GET().build()

    private fun postForm(url: String, fields: List<Pair<String, String>>) = HttpRequest.newBuilder(URI(url))
        .timeout(Duration.ofSeconds(15))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "text/html")
        .POST(HttpRequest.BodyPublishers.ofString(fields.joinToString("&") { (k, v) -> "${enc(k)}=${enc(v)}" }))
        .build()

    companion object {
        private val FORM = Regex("""<form\b([^>]*)>(.*?)</form>""", setOf(RegexOption.DOT_MATCHES_ALL, RegexOption.IGNORE_CASE))
        private val INPUT = Regex("""<input\b[^>]*>""", RegexOption.IGNORE_CASE)

        private fun enc(s: String) = URLEncoder.encode(s, Charsets.UTF_8)

        private fun attr(tag: String, name: String): String? =
            Regex("""\b$name\s*=\s*"([^"]*)"""", RegexOption.IGNORE_CASE).find(tag)?.groupValues?.get(1)?.let(::unescape)

        private fun unescape(s: String) = s.replace("&quot;", "\"").replace("&#039;", "'").replace("&#39;", "'")
            .replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")

        /** name -> value of every `<input type="hidden">` in [html] (first occurrence wins). */
        fun hiddenInputs(html: String): Map<String, String> = INPUT.findAll(html)
            .map { it.value }
            .filter { attr(it, "type").equals("hidden", ignoreCase = true) }
            .mapNotNull { tag -> attr(tag, "name")?.let { it to (attr(tag, "value") ?: "") } }
            .fold(LinkedHashMap<String, String>()) { m, (k, v) -> m.apply { putIfAbsent(k, v) } }

        /**
         * The consent page's approve form: (action, hidden fields) of the first form posting to `/oauth/authorize`
         * that is not the deny form (`_method=DELETE`).
         */
        fun approveForm(html: String): Pair<String, List<Pair<String, String>>>? = FORM.findAll(html)
            .mapNotNull { m ->
                val action = attr(m.groupValues[1], "action") ?: return@mapNotNull null
                val fields = hiddenInputs(m.groupValues[2])
                if (!action.substringBefore('?').endsWith("/oauth/authorize")) return@mapNotNull null
                if (fields["_method"].equals("DELETE", ignoreCase = true)) return@mapNotNull null
                action to fields.toList()
            }
            .firstOrNull()
    }
}
