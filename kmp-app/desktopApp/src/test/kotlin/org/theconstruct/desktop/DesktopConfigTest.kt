package org.theconstruct.desktop

import java.io.File
import kotlin.test.Test
import kotlin.test.assertEquals

class DesktopConfigTest {
    @Test
    fun overrideWinsOverSavedChoiceWhichWinsOverStaging() {
        assertEquals("http://127.0.0.1:9000", resolveBaseUrl("http://127.0.0.1:9000/", "http://127.0.0.1:8080"))
        assertEquals("http://127.0.0.1:8080", resolveBaseUrl(null, "http://127.0.0.1:8080"))
        assertEquals(DEFAULT_BASE_URL, resolveBaseUrl(null, null))
        // A saved value that is no longer a valid server URL is ignored.
        assertEquals(DEFAULT_BASE_URL, resolveBaseUrl(null, "ftp://nope"))
    }

    @Test
    fun stagingKeepsTheRootAndOtherServersGetTheirOwnDirectory() {
        val root = File("/tmp/construct-root")
        assertEquals(root, storageDirFor(root, DEFAULT_BASE_URL))
        assertEquals(root, storageDirFor(root, "$DEFAULT_BASE_URL/"))
        assertEquals(File(root, "servers/http___127.0.0.1_8080"), storageDirFor(root, "http://127.0.0.1:8080"))
    }

    @Test
    fun defaultAppIsPixelfed() {
        assertEquals("pixelfed_home", DEFAULT_WORKSPACE)
    }
}
