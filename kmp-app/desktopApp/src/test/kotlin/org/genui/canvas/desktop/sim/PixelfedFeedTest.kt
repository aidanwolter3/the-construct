package org.genui.canvas.desktop.sim

import androidx.compose.ui.geometry.Size
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.getBoundsInRoot
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.runSkikoComposeUiTest
import androidx.compose.ui.unit.Density
import kotlinx.serialization.json.JsonPrimitive
import org.genui.canvas.CanvasApp
import org.genui.canvas.CanvasController
import org.genui.canvas.ImageLoads
import java.io.File
import java.nio.file.Files
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** The `pixelfed_home` fixture (normalized fake-upstream timeline) through the real widgets, offscreen. */
@OptIn(ExperimentalTestApi::class)
class PixelfedFeedTest {
    private val outDir: File = Files.createTempDirectory("canvas-pixelfed-test").toFile()

    @AfterTest
    fun tearDown() {
        outDir.deleteRecursively()
    }

    private fun fake() = FakeEngineBridge(assertNotNull(findFixture("pixelfed_home_feed_bundle.json")).readText())

    private fun run(fake: FakeEngineBridge, script: String): SimResult {
        val r = SimRunner(CanvasController(fake), outDir, ScreenSpec.parse("phone"), setOnline = fake::setOnline)
            .run(parseScript(script))
        assertTrue(r.ok, r.steps.joinToString("\n") { it.logLine() })
        return r
    }

    @Test
    fun carouselShowsFirstImageWithDotsThenSwipesToSecond() {
        run(
            fake(),
            "wait-images 5000; assert-text Page 1 of 3; assert-text 1/3; " +
                "assert-text Red sky over the Flatirons, number 1; assert-no-text number 2; " +
                "swipe-left tag:carousel:$CAROUSEL; wait-images 5000; assert-text Page 2 of 3; assert-text 2/3; " +
                "assert-text Green meadow at first light, number 2; assert-no-text Red sky over the Flatirons; " +
                // Selecting by alt text works too: swipe the visible image back.
                "swipe-right Green meadow at first light; assert-text Page 1 of 3",
        )
        assertEquals(0, ImageLoads.inFlight.value)
    }

    @Test
    fun signedInStatusLineShowsUsernameAndSignOutSignsOut() {
        val fake = FakeEngineBridge(
            assertNotNull(findFixture("pixelfed_home_feed_bundle.json")).readText(),
            authJson = """{"state":"signed_in","username":"canvasdemo","display_name":"Canvas Demo"}""",
        )
        run(fake, "assert-text @canvasdemo; click Sign out; assert-no-text @canvasdemo; assert-no-text Sign out")
    }

    @Test
    fun pendingAndErrorAuthStatesShowInTheStatusLine() {
        val bundle = assertNotNull(findFixture("pixelfed_home_feed_bundle.json")).readText()
        run(FakeEngineBridge(bundle, authJson = """{"state":"pending"}"""), "assert-text Signing in; assert-no-text Sign out")
        run(
            FakeEngineBridge(bundle, authJson = """{"state":"error","error":"no browser available"}"""),
            "assert-text no browser available; assert-no-text Sign out",
        )
    }

    @Test
    fun autoApproveBrowserReadsTheRedirectUriAndRefusesUnreachableAuthorizePages() {
        assertEquals(
            "http://127.0.0.1:47823/callback",
            AutoApproveBrowser.queryParam(
                "http://h/oauth/authorize?client_id=1&redirect_uri=http%3A%2F%2F127.0.0.1%3A47823%2Fcallback&scope=read%20write",
                "redirect_uri",
            ),
        )
        assertNull(AutoApproveBrowser.queryParam("http://h/oauth/authorize?a=1", "redirect_uri"))
        // Nothing listens on port 9 (discard): the "browser" reports false so the engine logs the URL.
        val logs = mutableListOf<String>()
        val opened = AutoApproveBrowser { logs += it }
            .open("http://127.0.0.1:9/oauth/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A1%2Fcallback")
        assertEquals(false, opened)
        assertTrue(logs.any { "Open: http://127.0.0.1:9/oauth/authorize" in it }, logs.toString())
    }

    @Test
    fun likeFillsHeartAndCountsAndUnlikeRestores() {
        val fake = fake()
        run(fake, "assert-text 27; click tag:like:$CAROUSEL; assert-text 28; assert-no-text 27")
        assertEquals(JsonPrimitive(true), fake.entityField(CAROUSEL, "is_liked"))
        assertEquals(JsonPrimitive(28), fake.entityField(CAROUSEL, "like_count"))
        assertEquals(JsonPrimitive(28), fake.serverField(CAROUSEL, "like_count"))
        // The first plain "Like" button is that post's heart again; now it says Unlike.
        run(fake, "click Unlike; assert-text 27; assert-no-text 28")
        assertEquals(JsonPrimitive(false), fake.entityField(CAROUSEL, "is_liked"))
        assertEquals(JsonPrimitive(27), fake.entityField(CAROUSEL, "like_count"))
    }

    @Test
    fun bookmarkToggles() {
        val fake = fake()
        run(fake, "click tag:bookmark:$CAROUSEL")
        assertEquals(JsonPrimitive(true), fake.entityField(CAROUSEL, "is_bookmarked"))
        run(fake, "click tag:bookmark:$CAROUSEL")
        assertEquals(JsonPrimitive(false), fake.entityField(CAROUSEL, "is_bookmarked"))
    }

    @Test
    fun feedFollowsQueryOrderAndShowsBoostAndContentWarning() {
        run(fake(), "assert-text Bob 🚲 boosted; assert-text Tram lines, Lisbon.; assert-text CW: Snake photo")
        val controller = CanvasController(fake())
        SimImages.install("fixture")
        runSkikoComposeUiTest(size = Size(390f, 4000f), density = Density(1f)) {
            setContent { CanvasApp(controller) }
            waitForIdle()
            // The reblogged post (created Oct 29, the oldest) is second, as in queries, not last (D7 order).
            val tram = onNode(hasText("Tram lines", substring = true)).getBoundsInRoot().top.value
            val ride = onNode(hasText("Golden hour ride", substring = true)).getBoundsInRoot().top.value
            assertTrue(tram < ride, "tram $tram should be above ride $ride")
        }
    }

    @Test
    fun textOnlyPostHasNoMediaBox() {
        val controller = CanvasController(fake())
        SimImages.install("fixture")
        runSkikoComposeUiTest(size = Size(390f, 844f), density = Density(1f)) {
            setContent { CanvasApp(controller) }
            waitForIdle()
            onNode(hasTestTag("like:$TEXT_ONLY")).performScrollTo()
            waitForIdle()
            assertEquals(0, onAllNodes(hasTestTag("carousel:$TEXT_ONLY")).fetchSemanticsNodes().size)
            // Between the previous post's caption and this post's action bar there is only a header
            // (a media box would be at least 312 dp tall at the minimum 0.8 aspect on a 390 dp screen).
            val prevCaption = onNode(hasText("Legs are toast", substring = true)).getBoundsInRoot().bottom.value
            val like = onNode(hasTestTag("like:$TEXT_ONLY")).getBoundsInRoot().top.value
            assertTrue(like - prevCaption < 150f, "gap ${like - prevCaption} dp suggests a media box")
            onNode(hasText("Thanks @alice for the tips", substring = true)).assertExists()
        }
    }

    @Test
    fun fixtureInterceptorResolvesStorageAndProxyUrlsOnly() {
        val media = assertNotNull(findFixtureDir("pixelfed/media"))
        val i = FixtureMediaInterceptor(media)
        assertEquals(File(media, "carousel-2.png"), i.resolve("http://127.0.0.1:8081/storage/carousel-2.png"))
        assertEquals(File(media, "avatar-bob.png"), i.resolve("http://10.0.2.2:8080/v1/pixelfed/media/avatar-bob.png?x=1"))
        assertEquals(File(media, "avatar-bob.png"), i.resolve("http://h/v1/pixelfed/media/avatar-bob"))
        assertNull(i.resolve("http://h/v1/pixelfed/media/opaque-key-123"))
        assertNull(i.resolve("https://pixelfed.social/storage/501.webp"))
        assertNull(i.resolve("http://h/storage/.."))
        assertNull(FixtureMediaInterceptor(null).resolve("http://h/storage/carousel-1.png"))
    }

    @Test
    fun defaultFixtureForPixelfedHome() {
        val repo = assertNotNull(findFixture("pixelfed_home_feed_bundle.json")).parentFile.parentFile
        assertEquals("pixelfed_home_feed_bundle.json", defaultFixture("pixelfed_home", repo).name)
    }

    private companion object {
        const val CAROUSEL = "pixelfed://posts/781254316723458049"
        const val TEXT_ONLY = "pixelfed://posts/781220014433221633"
    }
}
