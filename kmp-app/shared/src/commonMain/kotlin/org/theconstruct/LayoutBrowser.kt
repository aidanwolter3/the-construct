package org.theconstruct

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.requiredSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.wrapContentSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.SmallFloatingActionButton
import androidx.compose.material3.Snackbar
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.Saver
import androidx.compose.runtime.saveable.listSaver
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.TransformOrigin
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonObject

// The layout browser (DECISIONS D30): a layouts button next to the AI FAB opens an in-tree, full-screen overlay
// (list of the workspace's layouts with live thumbnails -> a layout's details and version history -> one version's
// preview), confirmations as an in-tree dialog and an Undo snackbar. All shared Compose: desktop, Android, the
// simulator and a future web shell run the same code.

/** Width at which thumbnails and previews render the layout before scaling (a phone's width). */
private val RENDER_WIDTH = 390.dp
private val THUMB_WIDTH = 120.dp
private val THUMB_HEIGHT = 200.dp
private val CONTENT_MAX_WIDTH = 760.dp

/**
 * Items per feed query a thumbnail or preview renders: the scaled box shows one or two posts, and feeds are plain
 * Columns (every item composed and its images loaded), so N thumbnails x a long timeline must not mean N x all images.
 */
private const val PREVIEW_MAX_ITEMS = 3

/** How long the result snackbar stays (it also goes on the next change or a tap on Undo). */
private const val SNACK_MS = 8_000L

/** A list / history load. */
internal sealed class Load<out T> {
    object Loading : Load<Nothing>()
    data class Ready<T>(val value: T) : Load<T>()
    data class Failed(val message: String) : Load<Nothing>()
}

/** One layout's history as loaded so far (newest first). */
internal data class HistoryState(
    val versions: List<LayoutVersionInfo> = emptyList(),
    val nextBefore: Int? = null,
    val loaded: Boolean = false,
    val loadingMore: Boolean = false,
    val error: String? = null,
)

/**
 * The result snackbar: [undo] is offered as its action while the screen still shows [after] (the layout id and
 * version the change produced); [error] colors it as a failure.
 */
internal data class LayoutSnack(
    val message: String,
    val undo: LayoutChange?,
    val id: Long,
    val error: Boolean = false,
    val after: ActiveLayout? = null,
)

/**
 * Whether [snack]'s Undo still means "go back": false once the screen shows another layout or version than the change
 * produced (another device, an AI edit or a later change moved on; undoing would silently revert that instead).
 * Unknown values never block it.
 */
internal fun undoStillApplies(snack: LayoutSnack, now: ActiveLayout): Boolean {
    if (snack.undo == null) return false
    val after = snack.after ?: return true
    if (now.layoutId != null && after.layoutId != null && now.layoutId != after.layoutId) return false
    if (now.layoutVersion != null && after.layoutVersion != null && now.layoutVersion != after.layoutVersion) return false
    return true
}

/** Saves [LayoutNav] across rotation / process death as a list of primitives. */
internal val LayoutNavSaver: Saver<LayoutNav, Any> = listSaver(
    save = { listOf(it.open, it.layoutId ?: "", it.version ?: -1, it.confirming) },
    restore = { l ->
        LayoutNav.fromSaveable(
            listOf(l.getOrNull(0), (l.getOrNull(1) as? String)?.ifEmpty { null }, (l.getOrNull(2) as? Int)?.takeIf { it >= 0 }, l.getOrNull(3)),
        )
    },
)

/**
 * The browser's state: [nav], the loaded list and histories, the running write and the snackbar. Owned by
 * [ConstructController] (so on Android it lives in the ViewModel and survives rotation with the snackbar's Undo and a
 * write in flight); [ConstructApp] also saves [nav] for process death ([restoreNav]). Reads run in the composables'
 * effects (cancelled when their level closes); writes and undo are launched in the caller's (the app's) scope but run
 * to completion even if that scope ends (rotation), so their result is never dropped.
 */
@Stable
internal class LayoutBrowserState(
    val repo: LayoutRepository,
    /** What is on screen now, read when a change starts (its undo goes back to it). */
    private val currentActive: () -> ActiveLayout,
) {
    var nav: LayoutNav by mutableStateOf(LayoutNav())
    private var composedOnce = false

    /**
     * Called by [ConstructApp] on its first composition with the navigation saved before a process death (or the current one):
     * applied only to a fresh state, since after a rotation this state (kept by the controller) is already current.
     */
    fun restoreNav(saved: LayoutNav) {
        if (composedOnce) return
        composedOnce = true
        nav = saved
    }
    var list: Load<List<LayoutSummary>> by mutableStateOf(Load.Loading)
        private set
    /** Bumped by Retry: the list effect is keyed on it. */
    var listToken by mutableStateOf(0)
        private set
    val history = mutableStateMapOf<String, HistoryState>()
    /** Bumped by the history's Try again: the details' load effect is keyed on it. */
    var historyToken by mutableStateOf(0)
        private set
    var writing by mutableStateOf(false)
        private set
    var writeError by mutableStateOf<String?>(null)
        private set
    var snack by mutableStateOf<LayoutSnack?>(null)
    private var snackSeq = 0L

    fun layout(id: String?): LayoutSummary? = (list as? Load.Ready)?.value?.firstOrNull { it.id == id }

    fun open() {
        nav = nav.openBrowser()
    }

    fun back() {
        // Like the dialog's disabled Cancel: while its change runs, Back must not leave a level the result then closes.
        if (nav.confirming && writing) return
        if (nav.confirming) writeError = null
        nav = nav.back()
    }

    fun close() {
        writeError = null
        nav = nav.close()
    }

    fun retryList() {
        list = Load.Loading
        listToken++
    }

    fun dismissSnack() {
        snack = null
    }

    /** (Re)loads the list; a reload keeps showing the previous list until the new one arrives. */
    suspend fun loadList() {
        if (list !is Load.Ready) list = Load.Loading
        list = try {
            Load.Ready(repo.list())
        } catch (e: LayoutApiException) {
            if (list is Load.Ready) return // keep the stale list; the details show its own errors
            Load.Failed(e.message)
        }
    }

    /** Loads the newest page of [id]'s history (replacing what was loaded). */
    suspend fun loadHistory(id: String) {
        val cur = history[id] ?: HistoryState()
        history[id] = cur.copy(error = null)
        history[id] = try {
            val page = repo.versions(id)
            HistoryState(page.versions, page.nextBefore, loaded = true)
        } catch (e: LayoutApiException) {
            cur.copy(error = e.message, loadingMore = false)
        }
    }

    /** "Load older": appends the next page. */
    fun loadOlder(id: String, scope: CoroutineScope) {
        val cur = history[id] ?: return
        val before = cur.nextBefore ?: return
        if (cur.loadingMore) return
        history[id] = cur.copy(loadingMore = true, error = null)
        scope.launch {
            try {
                val page = repo.versions(id, before = before)
                val now = history[id] ?: return@launch
                // A reload of the newest page (a new version arrived) replaced the history meanwhile: drop this page.
                if (now.nextBefore != before) return@launch
                history[id] = now.copy(versions = now.versions + page.versions, nextBefore = page.nextBefore, loadingMore = false)
            } catch (e: LayoutApiException) {
                history[id]?.let { history[id] = it.copy(loadingMore = false, error = e.message) }
            } finally {
                // Also when the details closed mid-load (their scope is cancelled): never leave "Load older" spinning.
                history[id]?.let { if (it.loadingMore) history[id] = it.copy(loadingMore = false) }
            }
        }
    }

    fun retryHistory(id: String) {
        history[id] = HistoryState()
        historyToken++
    }

    /** Runs the confirmed [change]; on success closes the browser and offers Undo, on failure keeps the dialog open. */
    fun confirm(change: LayoutChange, scope: CoroutineScope) {
        if (writing) return
        val before = currentActive()
        writing = true
        writeError = null
        launchWrite(scope) {
            try {
                val outcome = repo.apply(change)
                nav = LayoutNav()
                history.remove(change.layoutId)
                before.layoutId?.let { history.remove(it) }
                snack = LayoutSnack(
                    changeMessage(change, outcome, repo.titleOf(outcome.layoutId)),
                    undoFor(change, before, outcome),
                    ++snackSeq,
                    after = ActiveLayout(outcome.layoutId, outcome.version),
                )
            } catch (e: LayoutApiException) {
                writeError = e.message
            }
        }
    }

    /** The snackbar's Undo: runs its change at once (append-only on the pod) and reports the result. */
    fun undo(scope: CoroutineScope) {
        val s = snack ?: return
        val change = s.undo ?: return
        if (writing) return
        snack = null
        if (!undoStillApplies(s, currentActive())) {
            snack = LayoutSnack("Not undone: the layout changed since.", null, ++snackSeq, error = true)
            return
        }
        writing = true
        launchWrite(scope) {
            try {
                val outcome = repo.apply(change)
                history.clear()
                snack = LayoutSnack(changeMessage(change, outcome, repo.titleOf(outcome.layoutId), isUndo = true), null, ++snackSeq)
            } catch (e: LayoutApiException) {
                snack = LayoutSnack("Couldn't undo: ${e.message}", null, ++snackSeq, error = true)
            }
        }
    }

    /**
     * Runs a write launched from [scope] to completion even when that scope ends meanwhile (an Android rotation
     * disposes the composition; this state outlives it), so its result and Undo still land; then clears [writing].
     */
    private fun launchWrite(scope: CoroutineScope, block: suspend () -> Unit) {
        scope.launch(start = CoroutineStart.UNDISPATCHED) {
            withContext(NonCancellable) {
                try {
                    block()
                } finally {
                    writing = false
                }
            }
        }
    }
}

/** The layouts button drawn above the AI FAB (tag `layouts-button`, "Browse layouts"). */
@Composable
internal fun LayoutsButton(onClick: () -> Unit) {
    SmallFloatingActionButton(
        onClick = onClick,
        containerColor = MaterialTheme.colorScheme.secondaryContainer,
        contentColor = MaterialTheme.colorScheme.onSecondaryContainer,
        modifier = Modifier.testTag("layouts-button"),
    ) {
        Icon(ConstructIcons.Dashboard, contentDescription = "Browse layouts", modifier = Modifier.size(20.dp))
    }
}

/** The browser overlay (when open) and the result snackbar, drawn by [ConstructApp] above everything else. */
@Composable
internal fun LayoutBrowserHost(st: LayoutBrowserState, state: ConstructState, modifier: Modifier = Modifier) {
    val nav = st.nav
    // Back closes the innermost level (dialog, preview, details, list); registered after the agent panel's handler.
    PlatformBackHandler(enabled = nav.open) { st.back() }
    // Reload the list whenever the browser opens, Retry is tapped or what is on screen changes (a switch, an AI edit).
    LaunchedEffect(nav.open, st.listToken, state.activeLayout) {
        if (nav.open) st.loadList()
    }
    Box(modifier) {
        AnimatedVisibility(
            visible = nav.open,
            enter = fadeIn() + slideInVertically { it / 12 },
            exit = fadeOut() + slideOutVertically { it / 12 },
        ) {
            LayoutBrowserScreen(st, state)
        }
        LayoutSnackbarHost(st, state.activeLayout, Modifier.align(Alignment.BottomCenter))
    }
}

@Composable
private fun LayoutBrowserScreen(st: LayoutBrowserState, state: ConstructState) {
    val nav = st.nav
    val scope = rememberCoroutineScope()
    // Surface blocks touches to the workspace below.
    Surface(Modifier.fillMaxSize().testTag("layouts-browser"), color = MaterialTheme.colorScheme.background) {
        Box(Modifier.fillMaxSize()) {
            Column(Modifier.fillMaxSize()) {
                val layout = st.layout(nav.layoutId)
                val (title, subtitle) = when (nav.level) {
                    LayoutNav.Level.Preview -> "Version ${nav.version}" to (layout?.title ?: "")
                    LayoutNav.Level.Details -> (layout?.title ?: "Layout") to (layout?.let { "Layout · ${it.currentVersion} version${if (it.currentVersion == 1) "" else "s"}" } ?: "")
                    else -> "Layouts" to "Choose how this screen looks"
                }
                BrowserTopBar(title, subtitle, showBack = nav.level != LayoutNav.Level.List, onBack = st::back, onClose = st::close)
                Box(Modifier.weight(1f).fillMaxWidth()) {
                    when (nav.level) {
                        LayoutNav.Level.List, LayoutNav.Level.Closed -> LayoutListScreen(st, state)
                        LayoutNav.Level.Details -> LayoutDetailsScreen(st, state, nav.layoutId!!)
                        LayoutNav.Level.Preview -> VersionPreviewScreen(st, state, nav.layoutId!!, nav.version!!)
                    }
                }
            }
            val layout = st.layout(nav.layoutId)
            val change = layout?.let { offeredChange(it, nav.version) }
            if (nav.confirming && layout != null && change != null) {
                ConfirmDialog(st, confirmCopy(change, layout), onConfirm = { st.confirm(change, scope) }, onCancel = st::back)
            }
        }
    }
}

@Composable
private fun BrowserTopBar(title: String, subtitle: String, showBack: Boolean, onBack: () -> Unit, onClose: () -> Unit) {
    Column {
        Row(
            Modifier.fillMaxWidth().heightIn(min = 64.dp).padding(horizontal = 4.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (showBack) {
                IconButton(onClick = onBack, modifier = Modifier.testTag("layouts-back")) {
                    Icon(ConstructIcons.ArrowBack, contentDescription = "Back")
                }
            } else {
                Spacer(Modifier.width(12.dp))
                Icon(ConstructIcons.Dashboard, contentDescription = null, tint = MaterialTheme.colorScheme.primary, modifier = Modifier.size(22.dp))
                Spacer(Modifier.width(12.dp))
            }
            Column(Modifier.weight(1f).padding(horizontal = 4.dp)) {
                Text(
                    title,
                    style = MaterialTheme.typography.titleLarge,
                    fontWeight = FontWeight.SemiBold,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.semantics { heading() },
                )
                if (subtitle.isNotEmpty()) {
                    Text(
                        subtitle,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            IconButton(onClick = onClose, modifier = Modifier.testTag("layouts-close")) {
                Icon(ConstructIcons.Close, contentDescription = "Close layouts")
            }
        }
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
    }
}

// ---- List -------------------------------------------------------------------------------------------------------

@Composable
private fun LayoutListScreen(st: LayoutBrowserState, state: ConstructState) {
    when (val l = st.list) {
        Load.Loading -> LoadingPanel("Loading layouts…")
        is Load.Failed -> ErrorPanel(l.message, onRetry = st::retryList)
        is Load.Ready -> if (l.value.isEmpty()) {
            MessagePanel("This screen has no layouts to choose from.")
        } else {
            BoxWithConstraints(Modifier.fillMaxSize()) {
                val columns = if (maxWidth >= 720.dp) 2 else 1
                Column(
                    Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    Column(Modifier.widthIn(max = CONTENT_MAX_WIDTH * columns / 2 + CONTENT_MAX_WIDTH / 2), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                        l.value.chunked(columns).forEach { row ->
                            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                                row.forEach { item ->
                                    LayoutCard(st, state, item, Modifier.weight(1f)) { st.nav = st.nav.selectLayout(item.id) }
                                }
                                repeat(columns - row.size) { Spacer(Modifier.weight(1f)) }
                            }
                        }
                        Spacer(Modifier.height(72.dp))
                    }
                }
            }
        }
    }
}

@Composable
private fun LayoutCard(st: LayoutBrowserState, state: ConstructState, item: LayoutSummary, modifier: Modifier, onClick: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    Surface(
        onClick = onClick,
        shape = RoundedCornerShape(16.dp),
        color = if (item.active) scheme.primaryContainer.copy(alpha = 0.35f) else scheme.surface,
        border = BorderStroke(if (item.active) 2.dp else 1.dp, if (item.active) scheme.primary else scheme.outlineVariant),
        modifier = modifier.testTag("layout-item-${item.id}"),
    ) {
        Row(Modifier.padding(12.dp)) {
            LayoutThumbnail(st, state, item, item.currentVersion, Modifier.size(THUMB_WIDTH, THUMB_HEIGHT))
            Spacer(Modifier.width(16.dp))
            Column(Modifier.weight(1f).heightIn(min = THUMB_HEIGHT), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(item.title, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                    if (item.active) Pill("Active", scheme.primary, scheme.onPrimary, ConstructIcons.Check)
                    Pill("v${item.activeVersion ?: item.currentVersion}", scheme.surfaceVariant, scheme.onSurfaceVariant)
                }
                if (item.description.isNotBlank()) {
                    Text(
                        item.description,
                        style = MaterialTheme.typography.bodyMedium,
                        color = scheme.onSurfaceVariant,
                        maxLines = 4,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                Spacer(Modifier.weight(1f))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        if (item.currentVersion == 1) "1 version" else "${item.currentVersion} versions",
                        style = MaterialTheme.typography.labelLarge,
                        color = scheme.primary,
                        modifier = Modifier.weight(1f),
                    )
                    Icon(ConstructIcons.ChevronRight, contentDescription = null, tint = scheme.primary, modifier = Modifier.size(20.dp))
                }
            }
        }
    }
}

@Composable
private fun Pill(text: String, container: Color, content: Color, icon: androidx.compose.ui.graphics.vector.ImageVector? = null) {
    Row(
        Modifier.clip(RoundedCornerShape(50)).background(container).padding(horizontal = 8.dp, vertical = 3.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (icon != null) {
            Icon(icon, contentDescription = null, tint = content, modifier = Modifier.size(14.dp))
            Spacer(Modifier.width(4.dp))
        }
        Text(text, style = MaterialTheme.typography.labelMedium, color = content, fontWeight = FontWeight.Medium)
    }
}

// ---- Thumbnails and previews ------------------------------------------------------------------------------------

/** A spec fetch for a thumbnail / preview: the node once loaded, or [failed]. */
private data class SpecLoad(val node: UiNode? = null, val failed: Boolean = false)

/**
 * [layout]'s [version] (the on-screen workspace when that is what is displayed, else fetched once per (id, version) and
 * cached) rendered live against the current entities, scaled into [modifier]'s size. A placeholder with the layout's
 * `preview_hint` shows while it loads or when it fails.
 */
@Composable
private fun LayoutThumbnail(st: LayoutBrowserState, state: ConstructState, layout: LayoutSummary, version: Int, modifier: Modifier) {
    val spec = rememberSpec(st, state, layout.id, version)
    Box(
        modifier.clip(RoundedCornerShape(12.dp))
            .background(MaterialTheme.colorScheme.surface)
            .border(1.dp, MaterialTheme.colorScheme.outlineVariant, RoundedCornerShape(12.dp))
            .semantics { contentDescription = "Preview of ${layout.title}" },
    ) {
        val node = spec.node
        if (node != null) {
            LayoutPreview(node, state.entities, state.queries, Modifier.fillMaxSize())
        } else {
            PreviewPlaceholder(layout.previewHint, loading = !spec.failed)
        }
    }
}

@Composable
private fun rememberSpec(st: LayoutBrowserState, state: ConstructState, id: String, version: Int): SpecLoad {
    val onScreen = state.activeLayout.layoutId == id && state.activeLayout.layoutVersion == version
    val initial = (if (onScreen) state.workspace else null) ?: st.repo.cachedSpec(id, version)
    val load by produceState(SpecLoad(initial), id, version, initial != null) {
        if (value.node == null) {
            value = try {
                SpecLoad(st.repo.spec(id, version))
            } catch (e: LayoutApiException) {
                SpecLoad(failed = true)
            }
        }
    }
    return load
}

@Composable
private fun PreviewPlaceholder(hint: String, loading: Boolean) {
    Column(
        Modifier.fillMaxSize().background(MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.6f)),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        if (loading) {
            CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.dp)
        } else {
            Icon(ConstructIcons.Dashboard, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(32.dp))
        }
        if (hint.isNotBlank()) {
            Spacer(Modifier.height(8.dp))
            Text(hint, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

/**
 * Renders [node] with the real [RenderUiNode] at [renderWidth] (at least the box's width; e.g. a phone's) and scales it
 * down to fit this box, clipped. Never interactive and never dispatches: intents go to a no-op, a pointer-swallowing
 * layer sits on top (taps reach the enclosing card instead), and its semantics are cleared so neither screen readers
 * nor tests see the inner nodes.
 */
@Composable
internal fun LayoutPreview(
    node: UiNode,
    entities: Map<String, JsonObject>,
    queries: Map<String, List<String>>,
    modifier: Modifier = Modifier,
    renderWidth: Dp = RENDER_WIDTH,
) {
    BoxWithConstraints(modifier.clipToBounds().clearAndSetSemantics { }) {
        val width = if (maxWidth > renderWidth) maxWidth else renderWidth
        val scale = maxWidth / width
        val height = maxHeight / scale
        Box(
            Modifier.wrapContentSize(Alignment.TopStart, unbounded = true)
                .requiredSize(width, height)
                .graphicsLayer {
                    scaleX = scale
                    scaleY = scale
                    transformOrigin = TransformOrigin(0f, 0f)
                },
        ) {
            val shown = remember(queries) { queries.mapValues { (_, uris) -> uris.take(PREVIEW_MAX_ITEMS) } }
            CompositionLocalProvider(LocalConstructQueries provides shown) {
                RenderUiNode(node, entities, onDispatch = NO_DISPATCH, modifier = Modifier.fillMaxSize())
            }
        }
        // Hit by every pointer event over the preview, so nothing inside it is; not consumed, so the card's click works.
        Box(
            Modifier.matchParentSize().pointerInput(Unit) {
                awaitPointerEventScope {
                    while (true) awaitPointerEvent(PointerEventPass.Initial)
                }
            },
        )
    }
}

/** Previews never act: a thumbnail or preview must not like a post or sign in. */
internal val NO_DISPATCH: (ActionIntent) -> Unit = { }

// ---- Details ----------------------------------------------------------------------------------------------------

@Composable
private fun LayoutDetailsScreen(st: LayoutBrowserState, state: ConstructState, id: String) {
    val layout = st.layout(id)
    val listLoad = st.list
    if (layout == null) {
        when (listLoad) {
            is Load.Failed -> ErrorPanel(listLoad.message, onRetry = st::retryList)
            is Load.Ready -> ErrorPanel("That layout doesn't exist.", onRetry = st::retryList)
            Load.Loading -> LoadingPanel("Loading layout…")
        }
        return
    }
    val hist = st.history[id]
    // Loads the newest page on entry, on Try again, and when the layout gained a version (a restore, an AI edit).
    LaunchedEffect(id, layout.currentVersion, st.historyToken) {
        val h = st.history[id]
        if (h == null || !h.loaded || h.versions.firstOrNull()?.version != layout.currentVersion) st.loadHistory(id)
    }
    val scope = rememberCoroutineScope()
    val scheme = MaterialTheme.colorScheme
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).testTag("layout-details"),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Column(Modifier.widthIn(max = CONTENT_MAX_WIDTH).fillMaxWidth().padding(16.dp)) {
            Row {
                LayoutThumbnail(st, state, layout, layout.currentVersion, Modifier.size(150.dp, 250.dp))
                Spacer(Modifier.width(16.dp))
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    Text(layout.title, style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold)
                    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        if (layout.active) Pill("Active", scheme.primary, scheme.onPrimary, ConstructIcons.Check)
                        Pill("v${layout.currentVersion}", scheme.surfaceVariant, scheme.onSurfaceVariant)
                    }
                    if (layout.description.isNotBlank()) {
                        Text(layout.description, style = MaterialTheme.typography.bodyMedium, color = scheme.onSurfaceVariant)
                    }
                    if (layout.active) {
                        Text(
                            "On screen now. Open a version below to preview or restore it.",
                            style = MaterialTheme.typography.bodySmall,
                            color = scheme.onSurfaceVariant,
                        )
                    } else {
                        Button(onClick = { st.nav = st.nav.confirm() }, modifier = Modifier.testTag("layout-use")) {
                            Text("Use this layout")
                        }
                    }
                }
            }
            Spacer(Modifier.height(24.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(ConstructIcons.History, contentDescription = null, tint = scheme.primary, modifier = Modifier.size(20.dp))
                Spacer(Modifier.width(8.dp))
                Text("Version history", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold, modifier = Modifier.semantics { heading() })
            }
            Text(
                "Every change is kept. Restoring adds a copy on top, so you can always go back.",
                style = MaterialTheme.typography.bodySmall,
                color = scheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp, bottom = 8.dp),
            )
            when {
                hist == null || (!hist.loaded && hist.error == null) -> LoadingRow()
                !hist.loaded -> InlineError(hist.error!!) { st.retryHistory(id) }
                else -> {
                    val now = nowEpochSeconds()
                    hist.versions.forEach { v ->
                        VersionRow(v, layout, now) { st.nav = st.nav.selectVersion(v.version) }
                    }
                    if (hist.error != null) InlineError(hist.error) { st.loadOlder(id, scope) }
                    if (hist.nextBefore != null) {
                        OutlinedButton(
                            onClick = { st.loadOlder(id, scope) },
                            enabled = !hist.loadingMore,
                            modifier = Modifier.padding(top = 8.dp).align(Alignment.CenterHorizontally).testTag("versions-more"),
                        ) {
                            if (hist.loadingMore) {
                                CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                                Spacer(Modifier.width(8.dp))
                            }
                            Text("Load older")
                        }
                    }
                }
            }
            Spacer(Modifier.height(72.dp))
        }
    }
}

@Composable
private fun VersionRow(v: LayoutVersionInfo, layout: LayoutSummary, now: Long, onClick: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    val current = v.version == layout.currentVersion
    Surface(
        onClick = onClick,
        shape = RoundedCornerShape(12.dp),
        color = if (current) scheme.secondaryContainer.copy(alpha = 0.5f) else Color.Transparent,
        modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp).testTag("version-item-${v.version}"),
    ) {
        Row(Modifier.padding(horizontal = 12.dp, vertical = 10.dp), verticalAlignment = Alignment.CenterVertically) {
            Box(
                Modifier.size(40.dp).clip(CircleShape).background(if (current) scheme.primary else scheme.surfaceVariant),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    "v${v.version}",
                    style = MaterialTheme.typography.labelLarge,
                    fontWeight = FontWeight.SemiBold,
                    color = if (current) scheme.onPrimary else scheme.onSurfaceVariant,
                )
            }
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                    SourceChip(v.source)
                    if (current) {
                        Text(
                            if (layout.active) "Current · on screen" else "Current",
                            style = MaterialTheme.typography.labelMedium,
                            color = scheme.primary,
                            fontWeight = FontWeight.SemiBold,
                        )
                    }
                }
                versionDetail(v)?.let {
                    Text(it, style = MaterialTheme.typography.bodyMedium, maxLines = 2, overflow = TextOverflow.Ellipsis)
                }
                val time = versionTimeLabel(v.createdAt, now)
                if (time.isNotEmpty()) {
                    Text(time, style = MaterialTheme.typography.bodySmall, color = scheme.onSurfaceVariant)
                }
            }
            Icon(ConstructIcons.ChevronRight, contentDescription = null, tint = scheme.onSurfaceVariant, modifier = Modifier.size(20.dp))
        }
    }
}

@Composable
private fun SourceChip(source: String) {
    val scheme = MaterialTheme.colorScheme
    val (container, content) = when (source) {
        "agent" -> scheme.tertiaryContainer to scheme.onTertiaryContainer
        "rollback", "restore" -> scheme.secondaryContainer to scheme.onSecondaryContainer
        else -> scheme.surfaceVariant to scheme.onSurfaceVariant
    }
    Pill(versionSourceLabel(source), container, content, if (source == "agent") ConstructIcons.AutoAwesome else null)
}

// ---- Version preview --------------------------------------------------------------------------------------------

@Composable
private fun VersionPreviewScreen(st: LayoutBrowserState, state: ConstructState, id: String, version: Int) {
    val layout = st.layout(id)
    if (layout == null) {
        LoadingPanel("Loading layout…")
        return
    }
    val info = st.history[id]?.versions?.firstOrNull { it.version == version }
    val spec = rememberSpec(st, state, id, version)
    val scheme = MaterialTheme.colorScheme
    Column(Modifier.fillMaxSize().testTag("version-preview"), horizontalAlignment = Alignment.CenterHorizontally) {
        if (info != null) {
            Row(
                Modifier.widthIn(max = CONTENT_MAX_WIDTH).fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                SourceChip(info.source)
                Spacer(Modifier.width(8.dp))
                Text(
                    listOfNotNull(versionDetail(info), versionTimeLabel(info.createdAt).ifEmpty { null }).joinToString(" · "),
                    style = MaterialTheme.typography.bodySmall,
                    color = scheme.onSurfaceVariant,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        Box(
            Modifier.weight(1f).fillMaxWidth().background(scheme.surfaceVariant.copy(alpha = 0.45f)).padding(16.dp),
            contentAlignment = Alignment.Center,
        ) {
            Box(
                Modifier.widthIn(max = 440.dp).fillMaxWidth().fillMaxHeight()
                    .clip(RoundedCornerShape(20.dp))
                    .background(scheme.background)
                    .border(1.dp, scheme.outlineVariant, RoundedCornerShape(20.dp))
                    .semantics { contentDescription = "Preview of ${layout.title} v$version" },
            ) {
                val node = spec.node
                if (node != null) {
                    LayoutPreview(node, state.entities, state.queries, Modifier.fillMaxSize(), renderWidth = 360.dp)
                } else if (spec.failed) {
                    ErrorPanel("Couldn't load this version.", onRetry = null)
                } else {
                    PreviewPlaceholder(layout.previewHint, loading = true)
                }
            }
        }
        HorizontalDivider(color = scheme.outlineVariant)
        Row(
            Modifier.widthIn(max = CONTENT_MAX_WIDTH).fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            when (val change = offeredChange(layout, version)) {
                null -> {
                    Icon(ConstructIcons.Check, contentDescription = null, tint = scheme.primary, modifier = Modifier.size(20.dp))
                    Spacer(Modifier.width(8.dp))
                    Text("This version is on screen now.", style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
                }
                is LayoutChange.Restore -> {
                    Text(
                        "Restoring adds a copy as v${layout.currentVersion + 1}.",
                        style = MaterialTheme.typography.bodySmall,
                        color = scheme.onSurfaceVariant,
                        modifier = Modifier.weight(1f).padding(end = 12.dp),
                    )
                    Button(onClick = { st.nav = st.nav.confirm() }, modifier = Modifier.testTag("version-restore")) {
                        Text("Restore this version")
                    }
                }
                is LayoutChange.Activate -> {
                    Text(
                        if (change.version == null) "${layout.title} isn't on screen." else "Adds a copy as v${layout.currentVersion + 1} and switches to it.",
                        style = MaterialTheme.typography.bodySmall,
                        color = scheme.onSurfaceVariant,
                        modifier = Modifier.weight(1f).padding(end = 12.dp),
                    )
                    Button(onClick = { st.nav = st.nav.confirm() }, modifier = Modifier.testTag("version-use")) {
                        Text(if (change.version == null) "Use this layout" else "Use this version")
                    }
                }
            }
        }
    }
}

// ---- Dialog, snackbar, states -----------------------------------------------------------------------------------

/** In-tree confirmation (not a platform Dialog window, so the headless harness captures it, like the agent panel). */
@Composable
private fun ConfirmDialog(st: LayoutBrowserState, copy: ConfirmCopy, onConfirm: () -> Unit, onCancel: () -> Unit) {
    Box(
        Modifier.fillMaxSize()
            .background(Color.Black.copy(alpha = 0.4f))
            .clickable(interactionSource = remember { MutableInteractionSource() }, indication = null, enabled = !st.writing, onClick = onCancel),
        contentAlignment = Alignment.Center,
    ) {
        Surface(
            shape = RoundedCornerShape(28.dp),
            color = MaterialTheme.colorScheme.surfaceContainerHigh,
            tonalElevation = 6.dp,
            shadowElevation = 8.dp,
            modifier = Modifier.padding(24.dp).widthIn(max = 420.dp).fillMaxWidth().testTag("confirm-dialog")
                // Taps inside the card must not fall through to the scrim (cancel).
                .clickable(interactionSource = remember { MutableInteractionSource() }, indication = null) { },
        ) {
            Column(Modifier.padding(24.dp)) {
                Text(copy.title, style = MaterialTheme.typography.headlineSmall, modifier = Modifier.semantics { heading() })
                Spacer(Modifier.height(16.dp))
                Text(copy.body, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                st.writeError?.let {
                    Spacer(Modifier.height(12.dp))
                    Text(
                        it,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.testTag("confirm-error").semantics { liveRegion = LiveRegionMode.Polite },
                    )
                }
                Spacer(Modifier.height(24.dp))
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End, verticalAlignment = Alignment.CenterVertically) {
                    TextButton(onClick = onCancel, enabled = !st.writing, modifier = Modifier.testTag("confirm-cancel")) { Text("Cancel") }
                    Spacer(Modifier.width(8.dp))
                    Button(onClick = onConfirm, enabled = !st.writing, modifier = Modifier.testTag("confirm-ok")) {
                        if (st.writing) {
                            CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp, color = MaterialTheme.colorScheme.onPrimary)
                            Spacer(Modifier.width(8.dp))
                        }
                        Text(if (st.writeError != null) "Try again" else copy.confirmLabel)
                    }
                }
            }
        }
    }
}

@Composable
private fun LayoutSnackbarHost(st: LayoutBrowserState, active: ActiveLayout, modifier: Modifier) {
    val scope = rememberCoroutineScope()
    val snack = st.snack
    // The last snack keeps showing while the exit animation runs.
    var last by remember { mutableStateOf(snack) }
    if (snack != null) last = snack
    LaunchedEffect(snack) {
        if (snack != null) {
            delay(SNACK_MS)
            if (st.snack == snack) st.dismissSnack()
        }
    }
    BoxWithConstraints(modifier.fillMaxWidth()) {
        // On a phone the snackbar stops short of the FABs (bottom end) so they stay reachable.
        val end = if (maxWidth < 600.dp) 88.dp else 16.dp
        AnimatedVisibility(
            visible = snack != null,
            enter = fadeIn() + slideInHorizontally { -it / 8 },
            exit = fadeOut() + slideOutHorizontally { -it / 8 },
            modifier = Modifier.align(Alignment.BottomCenter).padding(start = 16.dp, end = end, bottom = 16.dp).widthIn(max = 560.dp),
        ) {
            val shown = last ?: return@AnimatedVisibility
            Snackbar(
                // Undo only while the screen still shows what the change produced (see undoStillApplies).
                action = shown.undo?.takeIf { undoStillApplies(shown, active) }?.let {
                    {
                        TextButton(onClick = { st.undo(scope) }, modifier = Modifier.testTag("snackbar-undo")) {
                            Text("Undo", color = MaterialTheme.colorScheme.inversePrimary, fontWeight = FontWeight.SemiBold)
                        }
                    }
                },
                containerColor = if (shown.error) MaterialTheme.colorScheme.errorContainer else MaterialTheme.colorScheme.inverseSurface,
                contentColor = if (shown.error) MaterialTheme.colorScheme.onErrorContainer else MaterialTheme.colorScheme.inverseOnSurface,
                modifier = Modifier.testTag("layouts-snackbar").semantics { liveRegion = LiveRegionMode.Polite },
            ) {
                Text(shown.message)
            }
        }
    }
}

@Composable
private fun LoadingPanel(text: String) {
    Column(Modifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.Center) {
        CircularProgressIndicator()
        Spacer(Modifier.height(16.dp))
        Text(text, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun LoadingRow() {
    Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) {
        CircularProgressIndicator(Modifier.size(28.dp), strokeWidth = 3.dp)
    }
}

@Composable
private fun MessagePanel(text: String) {
    Box(Modifier.fillMaxSize().padding(32.dp), contentAlignment = Alignment.Center) {
        Text(text, style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/** A failed load: the readable message (e.g. "Sign in to see these layouts.") and Try again (`layouts-retry`). */
@Composable
private fun ErrorPanel(message: String, onRetry: (() -> Unit)?) {
    Column(
        Modifier.fillMaxSize().padding(32.dp).testTag("layouts-error"),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Icon(ConstructIcons.Warning, contentDescription = null, tint = MaterialTheme.colorScheme.error, modifier = Modifier.size(32.dp))
        Spacer(Modifier.height(12.dp))
        Text(message, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite })
        if (onRetry != null) {
            Spacer(Modifier.height(16.dp))
            OutlinedButton(onClick = onRetry, modifier = Modifier.testTag("layouts-retry")) { Text("Try again") }
        }
    }
}

@Composable
private fun InlineError(message: String, onRetry: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().padding(vertical = 8.dp).testTag("layouts-error"),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(ConstructIcons.Warning, contentDescription = null, tint = MaterialTheme.colorScheme.error, modifier = Modifier.size(20.dp))
        Spacer(Modifier.width(8.dp))
        Text(message, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.error, modifier = Modifier.weight(1f))
        TextButton(onClick = onRetry, modifier = Modifier.testTag("layouts-retry")) { Text("Try again") }
    }
}
