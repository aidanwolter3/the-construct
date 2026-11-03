package org.theconstruct.desktop.sim

import org.theconstruct.ConstructController
import java.io.File
import java.nio.file.Files
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * The comments screen of `layouts/pixelfed_home` (DECISIONS D34) through the generic screen support: the fixture
 * feed's layout declares it, the fake pod answers its data path from `fixtures/pixelfed_screen_data.json`.
 */
class ScreensSimTest {
    private val outDir: File = Files.createTempDirectory("construct-screens-test").toFile()

    @AfterTest
    fun tearDown() {
        outDir.deleteRecursively()
    }

    private fun fake(screenData: Boolean = true, formValues: FakeFormValues = FakeFormValues()) = FakeEngineBridge(
        assertNotNull(findFixture("pixelfed_home_feed_bundle.json")).readText(),
        screenData = if (screenData) assertNotNull(findFixture("pixelfed_screen_data.json")).readText() else null,
        formValues = formValues,
    )

    private fun run(fake: FakeEngineBridge, script: String): SimResult {
        val r = SimRunner(ConstructController(fake), outDir, ScreenSpec.parse("phone"), setOnline = fake::setOnline)
            .run(parseScript(script))
        assertTrue(r.ok, r.steps.joinToString("\n") { it.logLine() })
        return r
    }

    @Test
    fun theCommentButtonOpensTheCommentsScreenAndBackClosesIt() {
        run(
            fake(),
            "click tag:comment:$CAROUSEL; wait-text 3000 That second frame is unreal; assert-text Comments; " +
                "assert-text @bob thanks! Up at 5:40 for that one.; assert-text @bob · 16 h; assert-text Which lens?; " +
                "back; assert-no-text Which lens?; assert-text Sunrise over Boulder",
        )
        // The Back arrow too.
        run(fake(), "click tag:comment:$CAROUSEL; wait-text 3000 Which lens?; click tag:screen-back; assert-no-text Which lens?")
    }

    @Test
    fun addingACommentShowsItAtOnceThenAsSent() {
        val fake = fake()
        run(
            fake,
            "click tag:comment:$CAROUSEL; wait-text 3000 Which lens?; type tag:textfield:comment Golden light!; " +
                "click tag:textfield-send:comment; wait-text 3000 Golden light!; assert-text just now",
        )
        assertEquals(0, fake.pendingActions)
        assertTrue("Golden light!" in fake.serverDataJson(PATH)!!)
    }

    @Test
    fun offlineCommentsWaitThenSend() {
        val fake = fake()
        run(
            fake,
            "click tag:comment:$CAROUSEL; wait-text 3000 Which lens?; offline; type tag:textfield:comment From the tunnel; " +
                "click tag:textfield-send:comment; wait-text 3000 From the tunnel; assert-text Sending…; " +
                "online; wait-text 3000 just now; assert-no-text Sending…",
        )
        assertTrue("From the tunnel" in fake.serverDataJson(PATH)!!)
    }

    @Test
    fun aDataLoadErrorShowsWithTryAgain() {
        run(
            fake(screenData = false),
            "click tag:comment:$CAROUSEL; wait-text 3000 This isn't available anymore.; assert-text Try again; " +
                "click tag:screen-retry; assert-text This isn't available anymore.",
        )
    }

    /** The cards view (D48): the switch shows a two-column grid; a card opens the post screen; List goes back. */
    @Test
    fun cardsSwitchShowsTheGridAndACardOpensThePost() {
        run(
            fake(),
            "assert-no-text Open post by; click tag:choice:view:cards; wait-text 3000 Open post by Alice Lumen; " +
                "assert-no-text one coffee later; click tag:card:$CAROUSEL; wait-text 3000 one coffee later; " +
                "click tag:like:$CAROUSEL; assert-text Unlike; back; assert-text Open post by; " +
                "click tag:choice:view:; assert-no-text Open post by; assert-text one coffee later",
        )
    }

    /**
     * Schema 10: the List/Cards choice is remembered on the device. Each run is a fresh controller and engine (an app
     * restart) over the same remembered values.
     */
    @Test
    fun theChosenViewIsRememberedAcrossRestarts() {
        val device = FakeFormValues()
        run(fake(formValues = device), "assert-no-text Open post by; click tag:choice:view:cards; wait-text 3000 Open post by")
        assertEquals(mapOf("view" to "cards"), device.get("pixelfed_home", ""))
        run(fake(formValues = device), "assert-text Open post by Alice Lumen; assert-no-text one coffee later; click tag:choice:view:")
        assertEquals(emptyMap<String, String>(), device.get("pixelfed_home", ""))
        run(fake(formValues = device), "assert-no-text Open post by; assert-text one coffee later")
        // A remembered value the layout no longer offers is ignored (the default view shows).
        device.set("pixelfed_home", "", "view", "tiles")
        run(fake(formValues = device), "assert-no-text Open post by; assert-text one coffee later")
    }

    companion object {
        const val CAROUSEL = "pixelfed://posts/781254316723458049"
        const val PATH = "/v1/pixelfed/screens/comments?post_id=781254316723458049"
    }
}
