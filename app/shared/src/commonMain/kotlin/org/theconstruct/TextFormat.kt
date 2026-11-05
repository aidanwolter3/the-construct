package org.theconstruct

import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

// `core://Text` `format` and `count` (features `date_format` and `count`,
// docs/decisions/text-dates-and-counts.md). Drawn here, not on the pod: "tomorrow" depends on the viewer's clock and
// time zone, and a cached screen must still be right the next day.

private val ISO_DATE = Regex("""(\d{4})-(\d{2})-(\d{2})""")
private val COUNT_TOKEN = Regex("""\${'$'}count(?![A-Za-z0-9_])""")
private val MONTHS = listOf("Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec")

/** Days since 1970-01-01 of a `YYYY-MM-DD` date (exactly that, a real calendar day), else null. */
fun isoDateEpochDay(s: String): Long? {
    val m = ISO_DATE.matchEntire(s.trim()) ?: return null
    val (y, mo, d) = m.destructured.toList().map { it.toInt() }
    if (mo !in 1..12 || d < 1 || d > daysInMonth(y, mo)) return null
    return epochDay(y, mo, d)
}

private fun isLeap(y: Int) = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)

private fun daysInMonth(y: Int, m: Int) = when (m) {
    2 -> if (isLeap(y)) 29 else 28
    4, 6, 9, 11 -> 30
    else -> 31
}

/** Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's days_from_civil). */
private fun epochDay(year: Int, month: Int, day: Int): Long {
    val y = (if (month <= 2) year - 1 else year).toLong()
    val era = y.floorDiv(400L)
    val yoe = y - era * 400
    val mp = (month + 9) % 12
    val doy = (153 * mp + 2) / 5 + day - 1
    val doe = yoe * 365 + yoe / 4 - yoe / 100 + doy
    return era * 146_097 + doe - 719_468
}

/** `(year, month, day)` of an epoch day (civil_from_days). */
private fun civil(epochDay: Long): Triple<Int, Int, Int> {
    val z = epochDay + 719_468
    val era = z.floorDiv(146_097L)
    val doe = z - era * 146_097
    val yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365
    val doy = doe - (365 * yoe + yoe / 4 - yoe / 100)
    val mp = (5 * doy + 2) / 153
    val d = (doy - (153 * mp + 2) / 5 + 1).toInt()
    val m = (if (mp < 10) mp + 3 else mp - 9).toInt()
    val y = (yoe + era * 400 + if (m <= 2) 1 else 0).toInt()
    return Triple(y, m, d)
}

/**
 * [day] drawn as [format] seen on [today] (both epoch days): [DateFormat.Relative] is "today", "tomorrow",
 * "yesterday", "in N days" / "N days ago" up to six days, else as [DateFormat.Short]: "Nov 5", with the year when it
 * is not [today]'s year ("Nov 5, 2027").
 */
fun formatDate(day: Long, today: Long, format: DateFormat): String {
    if (format == DateFormat.Relative) {
        when (val diff = day - today) {
            0L -> return "today"
            1L -> return "tomorrow"
            -1L -> return "yesterday"
            in 2L..6L -> return "in $diff days"
            in -6L..-2L -> return "${-diff} days ago"
        }
    }
    val (y, m, d) = civil(day)
    val short = "${MONTHS[m - 1]} $d"
    return if (y == civil(today).first) short else "$short, $y"
}

/**
 * How many entities of [count]'s query match every one of its `where` conditions (as a list's `where`: `equals` may
 * be `$form.<field>`, a blank one matches everything); null while the query is not loaded.
 */
fun countEntities(
    count: TextCount,
    queries: Map<String, List<String>>,
    entities: Map<String, JsonObject>,
    form: Map<String, String>?,
): Int? {
    val items = listItems(count.queryKey, queries, entities) ?: return null
    val conditions = whereConditions(count.where, form)
    return items.count { matchesAll(conditions, it) }
}

/**
 * The text a `core://Text` draws ([interpolate], plus its `count` and `format`). [today] is the viewer's epoch day.
 * `$count` is replaced by [countEntities] ("" while the query is not loaded; a 0 draws `zero_text` when set). Then
 * every bound `YYYY-MM-DD` value is drawn with [formatDate]; when one is before [today] and the format has `past`,
 * `past` is drawn instead.
 */
fun formatText(
    node: UiNode.Text,
    entities: Map<String, JsonObject>,
    item: JsonObject?,
    queries: Map<String, List<String>>,
    form: Map<String, String>?,
    today: Long,
): String {
    var expr = node.valueExpr
    node.count?.let { c ->
        val n = countEntities(c, queries, entities, form)
        if (n == 0 && c.zeroText != null) return interpolate(c.zeroText, entities, item)
        expr = COUNT_TOKEN.replace(expr, n?.toString().orEmpty())
    }
    val format = node.format ?: return interpolate(expr, entities, item)
    var past = false
    val text = interpolate(expr, entities, item) { v ->
        val day = (v as? JsonPrimitive)?.takeIf { it.isString && it !is JsonNull }?.let { isoDateEpochDay(it.content) }
        if (day == null) {
            renderJsonValue(v)
        } else {
            if (day < today) past = true
            formatDate(day, today, format.date)
        }
    }
    return if (past && format.past != null) interpolate(format.past, entities, item) else text
}

/** Today's epoch day on this device, in its time zone. */
fun todayEpochDay(): Long = localEpochDay(nowEpochSeconds())
