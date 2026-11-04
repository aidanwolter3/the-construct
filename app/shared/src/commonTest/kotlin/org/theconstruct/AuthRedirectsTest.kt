package org.theconstruct

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

class AuthRedirectsTest {
    private val host = "the-construct-pod.fly.dev"
    private val link = "https://the-construct-pod.fly.dev/oauth/android"

    @Test
    fun appLinkOnlyForItsOwnHttpsServer() {
        assertEquals(link, AuthRedirects.appLinkFor("https://the-construct-pod.fly.dev", host))
        assertEquals(link, AuthRedirects.appLinkFor(" https://The-Construct-Pod.fly.dev/ ", host))
        for (other in listOf(
            "http://the-construct-pod.fly.dev",
            "https://the-construct-pod.fly.dev:8443",
            "https://evil.fly.dev",
            "https://the-construct-pod.fly.dev.evil.example",
            "http://10.0.2.2:8080",
            "http://127.0.0.1:8080",
            "not a url",
        )) {
            assertNull(AuthRedirects.appLinkFor(other, host), other)
        }
        assertNull(AuthRedirects.appLinkFor("https://the-construct-pod.fly.dev", ""))
    }

    @Test
    fun theAppLinkIsUsedOnlyOnceVerified() {
        assertEquals(link, AuthRedirects.forServer("https://the-construct-pod.fly.dev", host, appLinkVerified = true))
        assertEquals(
            AuthRedirects.CUSTOM_SCHEME,
            AuthRedirects.forServer("https://the-construct-pod.fly.dev", host, appLinkVerified = false),
        )
        assertEquals(AuthRedirects.CUSTOM_SCHEME, AuthRedirects.forServer("http://10.0.2.2:8080", host, appLinkVerified = true))
    }

    @Test
    fun redirectsTheAppAccepts() {
        for (ok in listOf(
            "construct://oauth",
            "construct://oauth?code=a&state=b",
            "construct://oauth/?code=a&state=b",
            "$link?code=a&state=b",
            "https://The-Construct-Pod.fly.dev/oauth/android?code=a",
        )) {
            assertTrue(AuthRedirects.isRedirect(ok, host), ok)
        }
        for (bad in listOf(
            "construct://oauthx?code=a",
            "construct://attachments/1",
            "https://evil.example/oauth/android?code=a",
            "https://the-construct-pod.fly.dev.evil.example/oauth/android?code=a",
            "https://the-construct-pod.fly.dev/oauth/android/x?code=a",
            "https://the-construct-pod.fly.dev/OAUTH/android?code=a",
            "http://the-construct-pod.fly.dev/oauth/android?code=a",
            "https://the-construct-pod.fly.dev:8443/oauth/android?code=a",
        )) {
            assertFalse(AuthRedirects.isRedirect(bad, host), bad)
        }
        assertFalse(AuthRedirects.isRedirect("$link?code=a", ""))
    }
}
