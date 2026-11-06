package org.theconstruct

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.MutableIntState
import androidx.compose.runtime.ProvidableCompositionLocal
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.put

// Plumbing (decision `plumbing-and-porcelain`): a layout whose root is a `core://Page` draws with no chrome from the
// app. What the app used to draw itself (the title, Back, a screen's spinner and error) is porcelain on the pod
// (`core:workspace`, `core:screen`), drawn from these nodes and this device data.

/** Where the layout being drawn is presented: `root`, `push`, `sheet`, `panel` or `screen`. */
data class NavContext(val presentation: String, val canGoBack: Boolean, val inFrame: Boolean = false)

/** The [NavContext] of the layout or screen being drawn (the root's by default). */
val LocalNav: ProvidableCompositionLocal<NavContext> = staticCompositionLocalOf { NavContext("root", canGoBack = false) }

/** The node being drawn has a bounded height to fill: a `core://Scroll` in it scrolls, rather than its parent. */
internal val LocalBoundedHeight: ProvidableCompositionLocal<Boolean> = staticCompositionLocalOf { false }

/** Counts the actions of the page being drawn, for `core://Scroll` `follow_end`. */
internal val LocalPageActions: ProvidableCompositionLocal<MutableIntState?> = staticCompositionLocalOf { null }

/**
 * `local://nav` (feature `nav_data`): `can_go_back`, `presentation` (root, push, sheet, panel, screen, or inline when
 * drawn inside another layout), `pushed` (presented as Push: the page draws its own Back), `inline` and `in_frame`
 * (in a sheet or panel).
 */
fun navEntity(nav: NavContext, inline: Boolean): JsonObject = buildJsonObject {
    put("can_go_back", nav.canGoBack && !inline)
    put("presentation", if (inline) "inline" else nav.presentation)
    put("pushed", !inline && nav.presentation == "push")
    put("inline", inline)
    put("in_frame", nav.inFrame)
}

/** `local://screen` (feature `nav_data`) of a screen whose data load is [load]: `loading`, `error` ("" when none). */
fun screenEntity(load: LoadState?): JsonObject = buildJsonObject {
    put("loading", load?.state == "loading")
    put("error", if (load?.state == "error") load.error ?: "Couldn't load this." else "")
}

/** The theme tokens and their light and dark defaults (`pod_core::porcelain::THEME_TOKENS`). */
val THEME_DEFAULTS: List<Triple<String, String, String>> = listOf(
    Triple("card_border", "#E4E4EA", "#3A3A42"),
    Triple("accent", "#4A2C82", "#D9C8FF"),
    Triple("accent_surface", "#EFE9FA", "#2E2645"),
    Triple("muted_surface", "#EEF0F4", "#2A2C31"),
    Triple("page", "#F7F5F0", "#1A1A1C"),
    Triple("like", "#ED4956", "#FF6B7A"),
    Triple("unread", "#1E66D0", "#7AB0FF"),
    Triple("error", "#B3261E", "#F2B8B5"),
)

/**
 * `local://theme` (feature `theme`): each token's colour for the app's mode, from the pod's theme ([pod], the
 * `core://theme` entity: `<token>` light, `<token>_dark`) or the defaults.
 */
fun themeEntity(pod: JsonObject?, dark: Boolean): JsonObject = buildJsonObject {
    for ((token, light, darkDefault) in THEME_DEFAULTS) {
        val key = if (dark) "${token}_dark" else token
        val set = (pod?.get(key) as? JsonPrimitive)?.contentOrNull?.takeIf { parseHexColor(it) != null }
        put(token, set ?: if (dark) darkDefault else light)
    }
}

/** The screens a layout opens by route: a workspace's or a page's. */
fun screensOf(node: UiNode?): List<Screen> = when (node) {
    is UiNode.Workspace -> node.screens
    is UiNode.Page -> node.screens
    else -> emptyList()
}

/** Whether [node] takes the rest of a bounded column's height (a scroll, a filling box, an inline layout, a gesture around one). */
internal fun fillsHeight(node: UiNode): Boolean = when (node) {
    is UiNode.Scroll -> true
    is UiNode.Box -> node.fill
    is UiNode.Layout -> node.presentation == EmbedPresentation.Inline && node.heightDp == null && node.showWhen == null
    is UiNode.Column -> node.children.any(::fillsHeight)
    is UiNode.Gesture -> fillsHeight(node.content)
    else -> false
}

/**
 * A plumbing root (a page's, or a page screen's) over the space given: no chrome, `local://nav` bound, and its actions
 * counted for `follow_end`. Inline in another layout it takes its content's height instead.
 */
@Composable
internal fun PlumbingRoot(
    root: UiNode,
    entities: Map<String, JsonObject>,
    onDispatch: (ActionIntent) -> Unit,
    modifier: Modifier,
    item: JsonObject? = null,
    inline: Boolean = false,
) {
    val nav = LocalNav.current
    val withNav = remember(entities, nav, inline) { entities + ("local://nav" to navEntity(nav, inline)) }
    val actions = remember { mutableIntStateOf(0) }
    val dispatch: (ActionIntent) -> Unit = {
        actions.intValue++
        onDispatch(it)
    }
    CompositionLocalProvider(
        LocalInlineWorkspace provides false,
        LocalBoundedHeight provides !inline,
        LocalFillLayout provides !inline,
        LocalPageActions provides actions,
    ) {
        RenderUiNode(root, withNav, dispatch, if (inline) modifier.fillMaxWidth() else modifier.fillMaxSize(), item)
    }
}

/** `core://Page`: its root ([PlumbingRoot]); its screens open through [Navigation]. */
@Composable
internal fun NodePage(
    node: UiNode.Page,
    entities: Map<String, JsonObject>,
    onDispatch: (ActionIntent) -> Unit,
    modifier: Modifier,
    item: JsonObject?,
) {
    PlumbingRoot(node.root, entities, onDispatch, modifier, item, inline = LocalInlineWorkspace.current)
}

/**
 * `core://Scroll`: its content scrolls in the height it is given, with `core://Sticky` pinning at its top. With
 * `follow_end`, an action from outside it (a footer's Send) makes it follow the content to its end once it grows.
 * Where the height isn't bounded (inside another scroll, inline), the content is drawn as it is.
 */
@Composable
internal fun NodeScroll(
    node: UiNode.Scroll,
    entities: Map<String, JsonObject>,
    onDispatch: (ActionIntent) -> Unit,
    item: JsonObject?,
    modifier: Modifier,
) {
    if (!LocalBoundedHeight.current) {
        CompositionLocalProvider(LocalFillLayout provides false) {
            RenderUiNode(node.content, entities, onDispatch, modifier, item)
        }
        return
    }
    val scroll = rememberScrollState()
    val sticky = remember(scroll) { StickyScroll(scroll) }
    val actions = LocalPageActions.current
    // An action from inside the scroll (a like) doesn't make it follow the end; one from outside (Send) does.
    val inside = remember { booleanArrayOf(false) }
    var follow by remember { mutableStateOf(false) }
    if (node.followEnd && actions != null) {
        val count = actions.intValue
        var seen by remember { mutableIntStateOf(count) }
        LaunchedEffect(count) {
            if (count != seen) {
                seen = count
                if (inside[0]) inside[0] = false else follow = true
            }
        }
        LaunchedEffect(scroll.maxValue, follow) {
            if (follow && scroll.maxValue > 0) {
                scroll.animateScrollTo(scroll.maxValue)
                follow = false
            }
        }
    }
    val dispatch: (ActionIntent) -> Unit = {
        inside[0] = true
        onDispatch(it)
    }
    Column(modifier.fillMaxWidth().verticalScroll(scroll).stickyContent(sticky)) {
        CompositionLocalProvider(
            LocalStickyScroll provides sticky,
            LocalBoundedHeight provides false,
            LocalFillLayout provides false,
        ) {
            RenderUiNode(node.content, entities, dispatch, Modifier.fillMaxWidth(), item)
        }
    }
}
