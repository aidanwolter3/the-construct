package org.theconstruct.desktop.sim

import com.sun.net.httpserver.HttpExchange
import com.sun.net.httpserver.HttpServer
import java.net.InetSocketAddress
import java.net.URLDecoder
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * [PixelfedLoginBrowser] against a stub that behaves like Pixelfed 0.15's web login + Passport consent page
 * (session cookie, CSRF token, e-mail login, approve/deny forms; markup copied from the real pages).
 */
class PixelfedLoginBrowserTest {
    private val servers = mutableListOf<HttpServer>()

    @AfterTest
    fun stop() = servers.forEach { it.stop(0) }

    private fun server(handler: (HttpExchange) -> Unit): String {
        val s = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0)
        s.createContext("/") { ex -> ex.use { handler(it) } }
        s.start()
        servers += s
        return "http://127.0.0.1:${s.address.port}"
    }

    private fun HttpExchange.reply(code: Int, body: String = "", vararg headers: Pair<String, String>) {
        headers.forEach { (k, v) -> responseHeaders.add(k, v) }
        val bytes = body.toByteArray()
        sendResponseHeaders(code, if (bytes.isEmpty()) -1 else bytes.size.toLong())
        if (bytes.isNotEmpty()) responseBody.write(bytes)
    }

    private fun form(ex: HttpExchange): Map<String, String> =
        ex.requestBody.readBytes().decodeToString().split('&').filter { it.isNotEmpty() }.associate {
            val (k, v) = it.split('=', limit = 2)
            URLDecoder.decode(k, Charsets.UTF_8) to URLDecoder.decode(v, Charsets.UTF_8)
        }

    private fun cookie(ex: HttpExchange) = ex.requestHeaders["Cookie"].orEmpty().joinToString(";")

    /** A fake Pixelfed; [approvedAlready] makes authorize redirect without the consent page. */
    private fun pixelfed(redirect: String, approvedAlready: Boolean = false): String = server { ex ->
        val path = ex.requestURI.path
        val loggedIn = "auth=yes" in cookie(ex)
        when {
            ex.requestMethod == "GET" && path == "/login" -> ex.reply(
                200,
                """<form method="POST" action="http://x/login">
                   <input type="hidden" name="_token" value="CSRF1" autocomplete="off">
                   <input id="email" type="email" name="email" value="" required></form>""",
                "Set-Cookie" to "pixelfed_session=s1; path=/; httponly",
            )
            ex.requestMethod == "POST" && path == "/login" -> {
                val f = form(ex)
                val ok = "pixelfed_session=s1" in cookie(ex) && f["_token"] == "CSRF1" &&
                    f["email"] == "constructadmin@example.test" && f["password"] == "pw"
                if (ok) ex.reply(302, "", "Location" to "http://x/i/web", "Set-Cookie" to "auth=yes; path=/")
                else ex.reply(302, "", "Location" to "http://x/login")
            }
            path == "/oauth/authorize" && !loggedIn -> ex.reply(302, "", "Location" to "http://x/login")
            ex.requestMethod == "GET" && path == "/oauth/authorize" -> {
                val state = AutoApproveBrowser.queryParam("http://x${ex.requestURI.rawPath}?${ex.requestURI.rawQuery}", "state")
                if (approvedAlready) {
                    ex.reply(302, "", "Location" to "$redirect?code=C0&state=$state")
                } else {
                    val base = "http://127.0.0.1:${ex.localAddress.port}"
                    ex.reply(
                        200,
                        """<form method="post" action="$base/oauth/authorize">
                        <input type="hidden" name="_token" value="CSRF2" autocomplete="off">                            <input type="hidden" name="_method" value="DELETE">                            <input type="hidden" name="state" value="$state">
                        <input type="hidden" name="client_id" value="1001">
                        <input type="hidden" name="auth_token" value="AT">
                        <button type="submit">Cancel</button></form>
                        <form method="post" action="$base/oauth/authorize">
                        <input type="hidden" name="_token" value="CSRF2" autocomplete="off">                            <input type="hidden" name="state" value="$state">
                        <input type="hidden" name="client_id" value="1001">
                        <input type="hidden" name="auth_token" value="AT">
                        <button type="submit">Authorize</button></form>""",
                    )
                }
            }
            ex.requestMethod == "POST" && path == "/oauth/authorize" -> {
                val f = form(ex)
                if (f["_method"] != null) ex.reply(302, "", "Location" to "$redirect?error=access_denied&state=${f["state"]}")
                else if (f["_token"] == "CSRF2" && f["auth_token"] == "AT" && f["client_id"] == "1001")
                    ex.reply(302, "", "Location" to "$redirect?code=C1&state=${f["state"]}")
                else ex.reply(419)
            }
            else -> ex.reply(404)
        }
    }

    private fun loopback(): Pair<String, CompletableFuture<String>> {
        val got = CompletableFuture<String>()
        val url = server { ex ->
            got.complete(ex.requestURI.rawQuery)
            ex.reply(200, "Signed in")
        }
        return "$url/callback" to got
    }

    private fun authorizeUrl(base: String, redirect: String) =
        "$base/oauth/authorize?response_type=code&client_id=1001&redirect_uri=" +
            java.net.URLEncoder.encode(redirect, Charsets.UTF_8) + "&scope=read%20write&state=ST&code_challenge=x&code_challenge_method=S256"

    @Test
    fun logsInByEmailApprovesAndFollowsTheRedirect() {
        val (redirect, got) = loopback()
        val base = pixelfed(redirect)
        val logs = mutableListOf<String>()
        // A bare username is sent as <user>@example.test (the dev seed's e-mail domain).
        assertTrue(PixelfedLoginBrowser("constructadmin", "pw") { logs += it }.open(authorizeUrl(base, redirect)), logs.toString())
        assertEquals("code=C1&state=ST", got.get(5, TimeUnit.SECONDS))
        assertTrue(logs.any { "consent approved" in it }, logs.toString())
    }

    @Test
    fun alreadyApprovedClientRedirectsStraightAway() {
        val (redirect, got) = loopback()
        val base = pixelfed(redirect, approvedAlready = true)
        assertTrue(PixelfedLoginBrowser("constructadmin@example.test", "pw") {}.open(authorizeUrl(base, redirect)))
        assertEquals("code=C0&state=ST", got.get(5, TimeUnit.SECONDS))
    }

    @Test
    fun wrongPasswordReportsFalseWithTheUrl() {
        val (redirect, got) = loopback()
        val base = pixelfed(redirect)
        val logs = mutableListOf<String>()
        val url = authorizeUrl(base, redirect)
        assertFalse(PixelfedLoginBrowser("constructadmin", "nope") { logs += it }.open(url))
        assertTrue(logs.any { "rejected" in it && "Open: $url" in it }, logs.toString())
        assertFalse(got.isDone)
    }

    @Test
    fun approveFormSkipsTheDenyForm() {
        val html = """<form method="post" action="http://h/oauth/authorize"><input type="hidden" name="_token" value="t">
            <input type="hidden" name="_method" value="DELETE"><input type="hidden" name="state" value="s"></form>
            <form method="post" action="http://h/oauth/authorize"><input type="hidden" name="_token" value="t">
            <input type="hidden" name="state" value="a&amp;b"><input type="hidden" name="client_id" value="7"></form>"""
        val (action, fields) = assertNotNull(PixelfedLoginBrowser.approveForm(html))
        assertEquals("http://h/oauth/authorize", action)
        assertEquals(listOf("_token" to "t", "state" to "a&b", "client_id" to "7"), fields)
        assertEquals(null, PixelfedLoginBrowser.approveForm("<p>nothing</p>"))
        assertEquals(mapOf("_token" to "x"), PixelfedLoginBrowser.hiddenInputs("""<input type="hidden" name="_token" value="x"><input name="email">"""))
    }
}
