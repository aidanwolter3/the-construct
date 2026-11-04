package org.theconstruct

import androidx.compose.runtime.Composable

/**
 * Handles the platform's system Back while [enabled] (Android: the Back button / gesture, via the activity's
 * `OnBackPressedDispatcher`). Desktop has no system Back; there it only reacts to [LocalBackPresses] (tests).
 */
@Composable
expect fun PlatformBackHandler(enabled: Boolean = true, onBack: () -> Unit)
