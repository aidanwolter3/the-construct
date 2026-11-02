package org.theconstruct

import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

internal actual val layoutIoDispatcher: CoroutineDispatcher = Dispatchers.IO

internal actual fun nowEpochSeconds(): Long = System.currentTimeMillis() / 1000

private val ABSOLUTE = DateTimeFormatter.ofPattern("MMM d, yyyy, HH:mm", Locale.getDefault())

internal actual fun formatAbsoluteTime(epochSeconds: Long): String =
    ABSOLUTE.format(Instant.ofEpochSecond(epochSeconds).atZone(ZoneId.systemDefault()))
