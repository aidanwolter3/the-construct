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

private fun local(epochSeconds: Long) = Instant.ofEpochSecond(epochSeconds).atZone(ZoneId.systemDefault())

private fun thisYear(epochSeconds: Long): Boolean = local(epochSeconds).year == local(nowEpochSeconds()).year

internal actual fun localEpochDay(epochSeconds: Long): Long = local(epochSeconds).toLocalDate().toEpochDay()

private val WEEKDAY = DateTimeFormatter.ofPattern("EEE", Locale.getDefault())
private val SHORT_DATE = DateTimeFormatter.ofPattern("MMM d", Locale.getDefault())
private val SHORT_DATE_YEAR = DateTimeFormatter.ofPattern("MMM d, yyyy", Locale.getDefault())
private val DAY_HEADER = DateTimeFormatter.ofPattern("EEEE, MMM d", Locale.getDefault())
private val DAY_HEADER_YEAR = DateTimeFormatter.ofPattern("EEEE, MMM d, yyyy", Locale.getDefault())

internal actual fun formatWeekdayShort(epochSeconds: Long): String = WEEKDAY.format(local(epochSeconds))

internal actual fun formatShortDate(epochSeconds: Long): String =
    (if (thisYear(epochSeconds)) SHORT_DATE else SHORT_DATE_YEAR).format(local(epochSeconds))

internal actual fun formatDayHeader(epochSeconds: Long): String =
    (if (thisYear(epochSeconds)) DAY_HEADER else DAY_HEADER_YEAR).format(local(epochSeconds))
