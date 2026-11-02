package org.genui.canvas

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.runtime.Composable
import androidx.compose.runtime.RememberObserver
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import coil3.compose.AsyncImage
import coil3.compose.AsyncImagePainter
import coil3.compose.LocalPlatformContext
import coil3.request.ImageRequest
import coil3.request.crossfade
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * Counts [CanvasImage]s that are on screen and have not finished loading (success or error). The simulator's
 * `wait-images` step waits for [inFlight] to reach 0, in fixture and live mode alike.
 */
object ImageLoads {
    private val _inFlight = MutableStateFlow(0)
    val inFlight: StateFlow<Int> = _inFlight.asStateFlow()

    internal fun add(delta: Int) = _inFlight.update { it + delta }
}

/**
 * One image's contribution to [ImageLoads]: pending from creation (so an image whose request has not
 * started yet still counts) until a terminal state or until it leaves the composition.
 */
private class ImageLoadTicket : RememberObserver {
    private var pending = false

    init {
        begin()
    }

    fun begin() {
        if (!pending) {
            pending = true
            ImageLoads.add(1)
        }
    }

    fun end() {
        if (pending) {
            pending = false
            ImageLoads.add(-1)
        }
    }

    override fun onRemembered() {}
    override fun onForgotten() = end()
    override fun onAbandoned() = end()
}

/**
 * Coil [AsyncImage] over a [placeholderColor] box: [placeholder] shows until the image arrives (which
 * crossfades in), [error] replaces it when loading fails or [url] is null. [contentDescription] is the
 * image's accessibility label (alt text); null marks it decorative.
 */
@Composable
internal fun CanvasImage(
    url: String?,
    contentDescription: String?,
    modifier: Modifier = Modifier,
    placeholderColor: Color = Color(0xFFE8E8EC),
    contentScale: ContentScale = ContentScale.Crop,
    placeholder: @Composable BoxScope.() -> Unit = {},
    error: @Composable BoxScope.() -> Unit = {},
) {
    Box(modifier.background(placeholderColor)) {
        if (url == null) {
            error()
            return@Box
        }
        val ticket = remember(url) { ImageLoadTicket() }
        var state by remember(url) { mutableStateOf<AsyncImagePainter.State>(AsyncImagePainter.State.Empty) }
        when (state) {
            is AsyncImagePainter.State.Success -> {}
            is AsyncImagePainter.State.Error -> error()
            else -> placeholder()
        }
        val context = LocalPlatformContext.current
        val request = remember(url, context) { ImageRequest.Builder(context).data(url).crossfade(true).build() }
        AsyncImage(
            model = request,
            contentDescription = contentDescription,
            modifier = Modifier.matchParentSize(),
            contentScale = contentScale,
            onState = { s ->
                state = s
                when (s) {
                    is AsyncImagePainter.State.Success, is AsyncImagePainter.State.Error -> ticket.end()
                    else -> ticket.begin()
                }
            },
        )
    }
}
