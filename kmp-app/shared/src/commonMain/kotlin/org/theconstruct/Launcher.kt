package org.theconstruct

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Icon
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.ProvidableCompositionLocal
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import kotlinx.serialization.json.JsonObject
import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.sin

// Launcher vocabulary (features `backdrop`, `clock`, `gestures`, `on_appear`): generic nodes a home-screen layout
// composes. Nothing here knows an app.

/** `core://Backdrop` defaults. */
internal val BACKDROP_BACKGROUND = Color(0xFF0A0C0F)
internal val BACKDROP_GRID = Color(0xFF7CC8DA)
internal const val BACKDROP_TILT_DEG = 36
internal const val BACKDROP_CELL_DP = 36
private val BACKDROP_TEXT = Color(0xFFE8ECEF)
private val BACKDROP_MUTED = Color(0xFF8B949C)
private val BACKDROP_OUTLINE = Color(0xFF2A333B)

/** The perspective distance of the grid (like CSS `perspective: 700px`). */
private const val GRID_FOCAL_DP = 700f

/** How far a drag must go to count as a `core://Swipe`. */
internal val SWIPE_THRESHOLD = 56.dp

/** True inside a `core://Backdrop`: some widgets draw their dark variant (muted text, the outlined add button). */
internal val LocalInBackdrop: ProvidableCompositionLocal<Boolean> = staticCompositionLocalOf { false }

/** The clock `core://Clock` reads, in epoch seconds (the simulator fixes it for repeatable screenshots). */
val LocalClockSeconds: ProvidableCompositionLocal<() -> Long> = staticCompositionLocalOf { { nowEpochSeconds() } }

/** The local time as a launcher shows it ("9:41"). */
internal expect fun formatClockTime(epochSeconds: Long): String

/** The weekday and date ("Wednesday, November 4"). */
internal expect fun formatClockDate(epochSeconds: Long): String

/**
 * The perspective grid of a `core://Backdrop` as line segments in px: a floor of [cell]-sized squares tilted back by
 * [tiltDeg] around the bottom edge of a [width] x [height] area, seen from [focal] px away (horizontal lines crowd
 * towards the top, vertical lines converge above the centre). Each segment is `(x0, y0, x1, y1)`.
 */
internal fun perspectiveGrid(width: Float, height: Float, cell: Float, tiltDeg: Int, focal: Float): List<FloatArray> {
    if (width <= 0f || height <= 0f || cell <= 1f) return emptyList()
    val t = Math.toRadians(tiltDeg.coerceIn(0, 60).toDouble())
    val s = sin(t).toFloat()
    val c = cos(t).toFloat()
    val cx = width / 2f
    // A point `d` up the floor (from the bottom edge) at offset `x` from the centre.
    fun scale(d: Float) = focal / (focal + d * s)
    fun y(d: Float) = height - d * c * scale(d)
    // Walk up the floor until the lines leave the top (or crowd closer than a pixel).
    var dMax = 0f
    while (y(dMax) > 0f && dMax < height * 20f) dMax += cell
    val out = mutableListOf<FloatArray>()
    var d = 0f
    var lastY = Float.MAX_VALUE
    while (d <= dMax) {
        val yy = y(d)
        if (lastY - yy >= 1f) {
            out += floatArrayOf(0f, yy, width, yy)
            lastY = yy
        }
        d += cell
    }
    // Vertical lines wide enough that they still cover the width where the floor is farthest (and smallest).
    val reach = (cx / scale(dMax)) + cell
    var x = 0f
    while (x <= reach) {
        for (sx in if (x == 0f) listOf(0f) else listOf(x, -x)) {
            out += floatArrayOf(cx + sx, height, cx + sx * scale(dMax), y(dMax))
        }
        x += cell
    }
    return out
}

/**
 * `core://Backdrop`: [UiNode.Backdrop.background] with the perspective grid and a vignette, then the content
 * (scrolling) and the footer (fixed at the bottom), all in a dark theme with the grid colour as the accent.
 */
@Composable
internal fun NodeBackdrop(
    node: UiNode.Backdrop,
    entities: Map<String, JsonObject>,
    onDispatch: (ActionIntent) -> Unit,
    item: JsonObject?,
    modifier: Modifier,
) {
    val background = parseHexColor(node.background) ?: BACKDROP_BACKGROUND
    val accent = parseHexColor(node.gridColor) ?: BACKDROP_GRID
    val tilt = node.tiltDeg ?: BACKDROP_TILT_DEG
    val cellPx = with(LocalDensity.current) { (node.cellDp ?: BACKDROP_CELL_DP).coerceIn(8, 200).dp.toPx() }
    val focalPx = with(LocalDensity.current) { GRID_FOCAL_DP.dp.toPx() }
    val scheme = darkColorScheme(
        primary = accent,
        onPrimary = background,
        secondary = accent,
        background = background,
        surface = background,
        surfaceVariant = Color(0xFF14181C),
        surfaceContainer = Color(0xFF14181C),
        surfaceContainerHigh = Color(0xFF181D22),
        onBackground = BACKDROP_TEXT,
        onSurface = BACKDROP_TEXT,
        onSurfaceVariant = BACKDROP_MUTED,
        outline = BACKDROP_OUTLINE,
        outlineVariant = BACKDROP_OUTLINE,
    )
    Box(modifier.fillMaxSize().background(background).testTag("backdrop")) {
        Canvas(Modifier.fillMaxSize()) {
            val line = accent.copy(alpha = 0.22f)
            val stroke = 0.8.dp.toPx()
            for (seg in perspectiveGrid(size.width, size.height, cellPx, tilt, focalPx)) {
                drawLine(line, Offset(seg[0], seg[1]), Offset(seg[2], seg[3]), strokeWidth = stroke)
            }
            drawRect(
                Brush.radialGradient(
                    0f to Color.Transparent,
                    0.62f to background.copy(alpha = 0.5f),
                    1f to background,
                    center = Offset(size.width / 2f, size.height * 0.55f),
                    radius = maxOf(size.width, size.height) * 0.75f,
                ),
            )
        }
        MaterialTheme(colorScheme = scheme, typography = MaterialTheme.typography, shapes = MaterialTheme.shapes) {
            CompositionLocalProvider(LocalContentColor provides BACKDROP_TEXT, LocalInBackdrop provides true) {
                Column(Modifier.fillMaxSize()) {
                    Column(Modifier.weight(1f).fillMaxWidth().verticalScroll(rememberScrollState())) {
                        RenderUiNode(node.content, entities, onDispatch, Modifier.fillMaxWidth(), item)
                    }
                    node.footer?.let { RenderUiNode(it, entities, onDispatch, Modifier.fillMaxWidth(), item) }
                }
            }
        }
    }
}

/** `core://Clock`: the time ("9:41", large and light) or the date, re-read every few seconds. */
@Composable
internal fun NodeClock(node: UiNode.Clock, modifier: Modifier) {
    val source = LocalClockSeconds.current
    var now by remember { mutableLongStateOf(source()) }
    LaunchedEffect(source) {
        while (true) {
            now = source()
            delay(5_000)
        }
    }
    when (node.format) {
        ClockFormat.Time -> Text(
            formatClockTime(now),
            fontSize = 64.sp,
            fontWeight = FontWeight.Light,
            letterSpacing = (-2).sp,
            lineHeight = 66.sp,
            modifier = modifier.testTag("clock-time"),
        )
        ClockFormat.Date -> Text(
            formatClockDate(now),
            fontSize = 15.sp,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = modifier.testTag("clock-date"),
        )
    }
}

/**
 * `core://Swipe`: watches vertical drags over its content without consuming them (taps and scrolling inside keep
 * working); a drag that goes [SWIPE_THRESHOLD] more up than sideways dispatches `on_swipe_up` (down: `on_swipe_down`),
 * once per gesture.
 */
@Composable
internal fun NodeSwipe(
    node: UiNode.Swipe,
    entities: Map<String, JsonObject>,
    onDispatch: (ActionIntent) -> Unit,
    dispatch: (ActionIntent) -> Unit,
    item: JsonObject?,
    modifier: Modifier,
) {
    val threshold = with(LocalDensity.current) { SWIPE_THRESHOLD.toPx() }
    val current by rememberUpdatedState(node)
    val send by rememberUpdatedState(dispatch)
    val mod = modifier.pointerInput(Unit) {
        awaitEachGesture {
            val down = awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
            var fired = false
            while (true) {
                val event = awaitPointerEvent(PointerEventPass.Initial)
                val change = event.changes.firstOrNull { it.id == down.id } ?: break
                if (!change.pressed) break
                if (fired) continue
                val dy = change.position.y - down.position.y
                val dx = change.position.x - down.position.x
                if (abs(dy) >= threshold && abs(dy) > abs(dx)) {
                    val intent = if (dy < 0) current.onSwipeUp else current.onSwipeDown
                    if (intent != null) {
                        fired = true
                        send(intent)
                    }
                }
            }
        }
    }
    RenderUiNode(node.content, entities, onDispatch, mod, item)
}

/** `core://OnAppear`: dispatches its intent once each time it enters the composition. */
@Composable
internal fun NodeOnAppear(node: UiNode.OnAppear, dispatch: (ActionIntent) -> Unit) {
    val send by rememberUpdatedState(dispatch)
    LaunchedEffect(node.intent) { send(node.intent) }
}

/** The add button of a launcher (`core://IconButton` of 44 dp or more inside a backdrop): an outlined accent circle. */
@Composable
internal fun BackdropCircleButton(
    icon: androidx.compose.ui.graphics.vector.ImageVector?,
    description: String,
    sizeDp: Int,
    onClick: () -> Unit,
    modifier: Modifier,
) {
    val accent = MaterialTheme.colorScheme.primary
    Box(
        modifier
            .size(sizeDp.dp)
            .clip(CircleShape)
            .background(MaterialTheme.colorScheme.background.copy(alpha = 0.6f))
            .border(1.5.dp, accent, CircleShape)
            .clickableWithRole(description, onClick),
        contentAlignment = Alignment.Center,
    ) {
        if (icon != null) {
            Icon(icon, contentDescription = null, tint = accent, modifier = Modifier.size((sizeDp * 0.4f).dp))
        } else {
            Text("+", color = accent, fontSize = (sizeDp * 0.4f).sp)
        }
    }
}

private fun Modifier.clickableWithRole(description: String, onClick: () -> Unit): Modifier =
    this.clickable(onClickLabel = description, role = Role.Button, onClick = onClick)
        .semantics { contentDescription = description }

/** Whether the app on screen is drawn edge to edge in the dark (a launcher home: its root hides the chrome). */
fun ConstructState.prefersDarkSystemBars(): Boolean = (workspace as? UiNode.Workspace)?.chrome == Chrome.Hidden
