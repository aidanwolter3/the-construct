package org.theconstruct

import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.staticCompositionLocalOf

/**
 * A stand-in for Android's back dispatcher on desktop, where there is no system Back. Null by default (then
 * [PlatformBackHandler] does nothing); tests and the simulator provide one and call [press].
 */
class BackPresses {
    private val handlers = ArrayList<() -> Unit>()

    internal fun add(h: () -> Unit) { handlers += h }
    internal fun remove(h: () -> Unit) { handlers -= h }

    /** Like a system Back: the most recently registered enabled handler runs. False when none is registered. */
    fun press(): Boolean {
        val h = handlers.lastOrNull() ?: return false
        h()
        return true
    }
}

val LocalBackPresses = staticCompositionLocalOf<BackPresses?> { null }

@Composable
actual fun PlatformBackHandler(enabled: Boolean, onBack: () -> Unit) {
    val presses = LocalBackPresses.current ?: return
    val current by rememberUpdatedState(onBack)
    DisposableEffect(presses, enabled) {
        val h: () -> Unit = { current() }
        if (enabled) presses.add(h)
        onDispose { presses.remove(h) }
    }
}
