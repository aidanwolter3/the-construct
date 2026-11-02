package org.theconstruct.desktop.sim

import org.theconstruct.Orientation
import org.theconstruct.SlotSpec
import org.theconstruct.TextStyle
import org.theconstruct.UiNode

/**
 * A tiny offline subset of the server's rules provider (M7 contract, "Rules provider") for the simulator's fixture
 * mode. Case-insensitive phrase matching, composable ("Hide post captions and put media at the very top" applies
 * both), walking the layout tree instead of hard-coding paths:
 * - `hide (post )?captions`: every PostCard caption becomes an empty Caption text (rendered with no space);
 * - `media at the (very )?top` / `photos first`: in every PostCard whose media slot holds the carousel, the carousel
 *   and the author header swap slots (PostCard renders author, media, action, caption in that order);
 * - `add a weather widget`: a placeholder slot `weather` at the top, a padded Container around a Caption text
 *   (idempotent).
 */
object FakeLayoutRules {
    const val PROVIDER = "fake-rules"
    /** Worded like client-core's message for the pod's `unsupported_prompt`. */
    const val UNSUPPORTED = "I can't do that yet. Try: Hide captions; Media at the very top; Add a weather widget"

    private val HIDE_CAPTIONS = Regex("""hide (post )?captions""", RegexOption.IGNORE_CASE)
    private val MEDIA_TOP = Regex("""media at the (very )?top|photos first""", RegexOption.IGNORE_CASE)
    private val WEATHER = Regex("""add a weather widget""", RegexOption.IGNORE_CASE)

    /** Like the server's rules provider: a caption or media rule on a layout without a PostCard is an error. */
    const val NO_CARD = "this layout has no pixelfed://PostCard"

    sealed interface Outcome {
        /** [layout] may equal the input (already in the requested state): the server then commits no version. */
        data class Applied(val layout: UiNode) : Outcome
        data class Failed(val message: String) : Outcome
    }

    /** The new layout, or null when no rule matches [prompt] or a matched rule cannot apply. */
    fun apply(prompt: String, layout: UiNode): UiNode? = (run(prompt, layout) as? Outcome.Applied)?.layout

    fun run(prompt: String, layout: UiNode): Outcome {
        if ((HIDE_CAPTIONS.containsMatchIn(prompt) || MEDIA_TOP.containsMatchIn(prompt)) && !hasPostCard(layout)) {
            return Outcome.Failed(NO_CARD)
        }
        var matched = false
        var out = layout
        if (HIDE_CAPTIONS.containsMatchIn(prompt)) {
            matched = true
            out = mapPostCards(out) { it.copy(captionSlot = UiNode.Text("", TextStyle.Caption)) }
        }
        if (MEDIA_TOP.containsMatchIn(prompt)) {
            matched = true
            out = mapPostCards(out) {
                if (it.mediaSlot is UiNode.MediaCarousel) it.copy(authorSlot = it.mediaSlot, mediaSlot = it.authorSlot) else it
            }
        }
        if (WEATHER.containsMatchIn(prompt)) {
            matched = true
            if (out is UiNode.Workspace && out.slots.none { it.slotId == "weather" }) {
                // The same slot the server's rules provider adds.
                val weather = SlotSpec("weather", "core://placeholder", WEATHER_CONTENT)
                out = out.copy(slots = listOf(weather) + out.slots)
            }
        }
        return if (matched) Outcome.Applied(out) else Outcome.Failed(UNSUPPORTED)
    }

    /** The server rules provider's weather placeholder: padded so the text does not touch the screen edge. */
    val WEATHER_CONTENT: UiNode = UiNode.Container(
        Orientation.Vertical,
        16,
        listOf(UiNode.Text("Weather widget (not available yet)", TextStyle.Caption)),
    )

    private fun hasPostCard(node: UiNode): Boolean = when (node) {
        is UiNode.Workspace -> node.slots.any { hasPostCard(it.content) }
        is UiNode.Container -> node.children.any { hasPostCard(it) }
        is UiNode.FeedList -> hasPostCard(node.itemTemplate)
        is UiNode.PostCard -> true
        else -> false
    }

    private fun mapPostCards(node: UiNode, f: (UiNode.PostCard) -> UiNode.PostCard): UiNode = when (node) {
        is UiNode.Workspace -> node.copy(slots = node.slots.map { it.copy(content = mapPostCards(it.content, f)) })
        is UiNode.Container -> node.copy(children = node.children.map { mapPostCards(it, f) })
        is UiNode.FeedList -> node.copy(itemTemplate = mapPostCards(node.itemTemplate, f))
        is UiNode.PostCard -> f(node)
        else -> node
    }
}
