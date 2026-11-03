package org.theconstruct.desktop.sim

import androidx.compose.ui.geometry.Size
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.getBoundsInRoot
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.test.runSkikoComposeUiTest
import androidx.compose.ui.unit.Density
import kotlinx.serialization.json.JsonPrimitive
import org.theconstruct.ConstructApp
import org.theconstruct.ConstructController
import org.theconstruct.ImageLoads
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
    private val outDir: File = Files.createTempDirectory("construct-pixelfed-test").toFile()

    @AfterTest
    fun tearDown() {
        outDir.deleteRecursively()
    }

    private fun fake() = FakeEngineBridge(assertNotNull(findFixture("pixelfed_home_feed_bundle.json")).readText())

    private fun run(fake: FakeEngineBridge, script: String): SimResult {
        val r = SimRunner(ConstructController(fake), outDir, ScreenSpec.parse("phone"), setOnline = fake::setOnline)
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
    fun carouselPageSurvivesSnapshotUpdates() {
        // A new snapshot (here: liking the post, which rebuilds entities and feed items) must not reset the
        // per-post pager state: items are keyed by entity URI.
        run(
            fake(),
            "swipe-left tag:carousel:$CAROUSEL; assert-text Page 2 of 3; " +
                "click tag:like:$CAROUSEL; assert-text 28; assert-text Page 2 of 3; " +
                "click tag:bookmark:$CAROUSEL; assert-text Page 2 of 3",
        )
    }

    @Test
    fun signedInSettingsListThePixelfedSignInAndSignOutSignsOut() {
        val fake = FakeEngineBridge(
            assertNotNull(findFixture("pixelfed_home_feed_bundle.json")).readText(),
            authJson = """{"state":"signed_in","username":"constructdemo","display_name":"Construct Demo"}""",
        )
        run(
            fake,
            "assert-no-text @constructdemo; open-menu; assert-no-text @constructdemo; " +
                "open-settings; assert-text @constructdemo · 127.0.0.1:8081; click tag:sign-in-pixelfed; " +
                "assert-text Used by; assert-text Sign out of Pixelfed; click tag:menu-sign-out; " +
                "assert-no-text @constructdemo; assert-no-text Sign out; assert-text Not signed in",
        )
    }

    @Test
    fun pendingAndErrorAuthStatesShowInTheMenu() {
        val bundle = assertNotNull(findFixture("pixelfed_home_feed_bundle.json")).readText()
        run(FakeEngineBridge(bundle, authJson = """{"state":"pending"}"""), "open-settings; assert-text Signing in; assert-no-text Sign out")
        run(
            FakeEngineBridge(bundle, authJson = """{"state":"error","error":"no browser available"}"""),
            "assert-no-text no browser available; open-settings; assert-text no browser available; assert-no-text Sign out",
        )
    }

    @Test
    fun pendingShowsTheEngineProgressMessage() {
        val bundle = assertNotNull(findFixture("pixelfed_home_feed_bundle.json")).readText()
        run(
            FakeEngineBridge(bundle, authJson = """{"state":"pending","message":"Loading your feed…"}"""),
            "open-settings; assert-text Loading your feed; assert-no-text Signing in; assert-no-text Sign out",
        )
    }

    @Test
    fun longSignInErrorWrapsInTheMenuInsteadOfBeingCut() {
        val bundle = assertNotNull(findFixture("pixelfed_home_feed_bundle.json")).readText()
        val reason = "This pod belongs to a different Pixelfed account."
        val controller = ConstructController(FakeEngineBridge(bundle, authJson = """{"state":"error","error":"$reason"}"""))
        SimImages.install("fixture")
        runSkikoComposeUiTest(size = Size(390f, 844f), density = Density(1f)) {
            setContent { ConstructApp(controller) }
            controller.menu.showSettings()
            waitForIdle()
            val layouts = mutableListOf<TextLayoutResult>()
            onNode(hasText(reason, substring = true))
                .performSemanticsAction(SemanticsActions.GetTextLayoutResult) { it(layouts) }
            val layout = layouts.single()
            assertTrue(layout.lineCount in 2..3, "wrapped (${layout.lineCount} lines)")
            assertTrue(!layout.hasVisualOverflow, "the reason is not cut off")
        }
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
    fun clickScrollsAnOffScreenTargetIntoViewFirst() {
        // The text-only post is below the fold on a phone; a click at its (off-screen) center would hit
        // nothing. The step scrolls it into view, then clicks, and says so.
        val fake = fake()
        val r = run(fake, "click tag:like:$TEXT_ONLY; assert-text 16; click tag:like:$CAROUSEL; assert-text 28")
        assertTrue("scrolled into view" in r.steps[0].message, r.steps[0].message)
        assertEquals(JsonPrimitive(true), fake.entityField(TEXT_ONLY, "is_liked"))
        // Now the first post's heart is above the viewport: scrolled back to it.
        assertTrue("scrolled into view" in r.steps[2].message, r.steps[2].message)
        assertEquals(JsonPrimitive(true), fake.entityField(CAROUSEL, "is_liked"))
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
        val controller = ConstructController(fake())
        SimImages.install("fixture")
        runSkikoComposeUiTest(size = Size(390f, 4000f), density = Density(1f)) {
            setContent { ConstructApp(controller) }
            waitForIdle()
            // The reblogged post (created Oct 29, the oldest) is second, as in queries, not last (D7 order).
            val tram = onNode(hasText("Tram lines", substring = true)).getBoundsInRoot().top.value
            val ride = onNode(hasText("Golden hour ride", substring = true)).getBoundsInRoot().top.value
            assertTrue(tram < ride, "tram $tram should be above ride $ride")
        }
    }

    @Test
    fun textOnlyPostHasNoMediaBox() {
        val controller = ConstructController(fake())
        SimImages.install("fixture")
        runSkikoComposeUiTest(size = Size(390f, 844f), density = Density(1f)) {
            setContent { ConstructApp(controller) }
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
