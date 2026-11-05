package org.theconstruct

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.relocation.BringIntoViewRequester
import androidx.compose.foundation.relocation.bringIntoViewRequester
import androidx.compose.material3.LocalContentColor
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.draw.clip
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.IconButton
import androidx.compose.material3.Icon
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.ProvidableCompositionLocal
import androidx.compose.runtime.key
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/** The most lines a `core://TextField` grows to (its `lines` is clamped to this). */
private const val MAX_TEXT_FIELD_LINES = 12

/**
 * How far below the top of the workspace pinned `core://Sticky` content stops: clear of the menu and bell buttons
 * (40 dp, 8 dp from the top) that [ConstructApp] draws over it.
 */
val STICKY_WORKSPACE_INSET = 56.dp

/** The diameter of a `core://Progress` spinner without `size_dp`. */
const val PROGRESS_DEFAULT_SIZE_DP = 16

/** Maximum width of a list item on wide screens (a readable column, like Instagram/Pixelfed web). */
val FEED_MAX_WIDTH = 560.dp

/** The workspace's queries (`query_key` -> ordered entity URIs), read by `core://List`. Provided by [ConstructApp]. */
val LocalConstructQueries: ProvidableCompositionLocal<Map<String, List<String>>> = staticCompositionLocalOf { emptyMap() }

/**
 * Today's epoch day for `core://Text` `format` (feature `date_format`); null (the default) reads the device's clock
 * and time zone each time the text draws. Tests and the simulator's `-Ptoday` pin it.
 */
val LocalToday: ProvidableCompositionLocal<Long?> = staticCompositionLocalOf { null }

/**
 * Extra start padding for the workspace title, so it sits beside the menu button [ConstructApp] draws over the top-left
 * corner instead of under it. 0 outside [ConstructApp].
 */
val LocalWorkspaceTitleInset: ProvidableCompositionLocal<Dp> = staticCompositionLocalOf { 0.dp }

/** Extra end padding for the workspace title, so it stays clear of the bell [ConstructApp] draws top-right. */
val LocalWorkspaceTitleEndInset: ProvidableCompositionLocal<Dp> = staticCompositionLocalOf { 0.dp }

/**
 * Renders one [UiNode]. [item] is the current item context inside a list or carousel template (the entity's
 * fields plus a synthetic `uri` field holding its entity key). Intents
 * declared in the tree are bound to [item] via [bindIntent] before reaching [onDispatch].
 * Unknown node types render a small placeholder and never crash.
 */
@Composable
fun RenderUiNode(
    node: UiNode,
    entities: Map<String, JsonObject>,
    onDispatch: (ActionIntent) -> Unit,
    modifier: Modifier = Modifier,
    item: JsonObject? = null,
) {
    val dispatch: (ActionIntent) -> Unit = { onDispatch(bindIntent(it, item)) }
    when (node) {
        is UiNode.Workspace -> if (LocalInlineWorkspace.current) {
            // Inline in another layout's page (`core://Layout`, decision `layouts-not-apps`): its slots only, at their
            // own height; the page around them scrolls.
            Column(modifier) {
                CompositionLocalProvider(LocalInlineWorkspace provides false) {
                    node.slots.forEach { slot ->
                        key(slot.slotId) { RenderUiNode(slot.content, entities, onDispatch, Modifier, item) }
                    }
                }
            }
        } else if (fillsScreen(node)) {
            // One slot that fills the screen itself (decisions `blueprints-and-home`, `layouts-not-apps`): no title
            // bar, no page scroll.
            CompositionLocalProvider(LocalFillLayout provides true) {
                RenderUiNode(node.slots[0].content, entities, onDispatch, modifier.fillMaxSize(), item)
            }
        } else {
            val scroll = rememberScrollState()
            val title: @Composable () -> Unit = {
                Text(
                    node.title,
                    style = MaterialTheme.typography.headlineMedium,
                    modifier = Modifier
                        .padding(start = 16.dp + LocalWorkspaceTitleInset.current, end = 16.dp + LocalWorkspaceTitleEndInset.current, top = 12.dp, bottom = 12.dp)
                        .testTag("workspace-title"),
                )
            }
            val slots: @Composable (StickyScroll) -> Unit = { sticky ->
                CompositionLocalProvider(LocalStickyScroll provides sticky) {
                    node.slots.forEach { slot ->
                        // Keyed by slot id: a layout change that inserts a slot above (e.g. a new version adding a
                        // banner at the top) keeps the state of the slots below (feed pager positions).
                        key(slot.slotId) {
                            RenderUiNode(slot.content, entities, onDispatch, Modifier, item)
                        }
                    }
                }
                Spacer(Modifier.height(16.dp))
            }
            if (node.titleScrolls) {
                // Feature `title_scrolls`: the title is the first thing in the scrolling view, and `core://Sticky`
                // content pins below the menu and bell buttons drawn over the top corners.
                val sticky = remember(scroll) { StickyScroll(scroll, topInset = STICKY_WORKSPACE_INSET) }
                Column(modifier.fillMaxSize().verticalScroll(scroll).stickyContent(sticky)) {
                    title()
                    slots(sticky)
                }
            } else {
                // The title is pinned in a bar beside the menu and bell buttons (decision `pinned-app-title`); only
                // the slots scroll, so `core://Sticky` content pins right under the title.
                val sticky = remember(scroll) { StickyScroll(scroll) }
                Column(modifier.fillMaxSize()) {
                    title()
                    Column(Modifier.fillMaxWidth().weight(1f).verticalScroll(scroll).stickyContent(sticky)) {
                        slots(sticky)
                    }
                }
            }
        }

        is UiNode.Container -> {
            val mod = modifier.padding(node.paddingDp.dp)
            when (node.orientation) {
                Orientation.Vertical -> Column(mod, verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    node.children.forEach { RenderUiNode(it, entities, onDispatch, Modifier, item) }
                }
                // FlowRow, not Row: children keep their natural width and wrap onto the next line
                // when they do not fit (a Row squeezes the last child, e.g. a button label wrapping
                // one letter per line on a phone).
                Orientation.Horizontal -> FlowRow(
                    mod,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                    itemVerticalAlignment = Alignment.CenterVertically,
                ) {
                    node.children.forEach { RenderUiNode(it, entities, onDispatch, Modifier, item) }
                }
            }
        }

        is UiNode.Text -> Text(
            text = if (node.format == null && node.count == null) {
                interpolate(node.valueExpr, entities, item)
            } else {
                // Read only by texts that need them: the form recomposes as it is typed into.
                formatText(
                    node, entities, item,
                    queries = if (node.count != null) LocalConstructQueries.current else emptyMap(),
                    form = if (node.count != null) LocalForm.current else null,
                    today = LocalToday.current ?: todayEpochDay(),
                )
            },
            style = when (node.style) {
                TextStyle.Headline -> MaterialTheme.typography.headlineMedium
                TextStyle.Title -> MaterialTheme.typography.titleLarge
                TextStyle.Body -> MaterialTheme.typography.bodyMedium
                TextStyle.Caption -> MaterialTheme.typography.labelSmall
                TextStyle.Label -> MaterialTheme.typography.titleSmall.copy(fontWeight = FontWeight.SemiBold)
                TextStyle.Secondary -> MaterialTheme.typography.bodySmall
                TextStyle.Display -> MaterialTheme.typography.headlineSmall.copy(
                    fontFamily = FontFamily.Serif,
                    fontWeight = FontWeight.SemiBold,
                )
            },
            color = if (node.style == TextStyle.Secondary) MutedText else Color.Unspecified,
            modifier = modifier,
        )

        is UiNode.Button -> Button(onClick = { dispatch(node.onClick) }, modifier = modifier) {
            Text(interpolate(node.label, entities, item))
        }

        is UiNode.ItemList -> ItemList(node, entities, onDispatch, modifier)

        is UiNode.Image -> NodeImage(node, entities, item, modifier)

        is UiNode.TextField -> FormTextField(node, dispatch, modifier)

        is UiNode.Row -> Row(
            modifier.padding(horizontal = node.paddingHDp.dp, vertical = node.paddingVDp.dp),
            horizontalArrangement = Arrangement.spacedBy(node.spacingDp.dp),
            verticalAlignment = when (node.align) {
                Align.Start -> Alignment.Top
                Align.End -> Alignment.Bottom
                Align.Center, null -> Alignment.CenterVertically
            },
        ) {
            node.children.forEach { child ->
                if (child is UiNode.Spacer && child.sizeDp == null) {
                    Spacer(Modifier.weight(1f))
                } else {
                    RenderUiNode(child, entities, onDispatch, Modifier, item)
                }
            }
        }

        is UiNode.Column -> Column(
            modifier.fillMaxWidth().padding(horizontal = node.paddingHDp.dp, vertical = node.paddingVDp.dp),
            verticalArrangement = Arrangement.spacedBy(node.spacingDp.dp),
            horizontalAlignment = when (node.align) {
                Align.Start -> Alignment.Start
                Align.Center -> Alignment.CenterHorizontally
                Align.End -> Alignment.End
            },
        ) {
            node.children.forEach { RenderUiNode(it, entities, onDispatch, Modifier, item) }
        }

        is UiNode.Spacer -> Spacer(modifier.size((node.sizeDp ?: 0).dp))

        UiNode.Divider -> HorizontalDivider(modifier, color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f))

        is UiNode.Conditional -> if (conditionShows(node.value, node.equals, item, LocalForm.current, entities)) {
            RenderUiNode(node.content, entities, onDispatch, modifier, item)
        }

        is UiNode.Carousel -> NodeCarousel(node, entities, onDispatch, item, modifier)

        is UiNode.IconButton -> NodeIconButton(node, item, dispatch, modifier)

        is UiNode.Chip -> NodeChip(node, entities, item, modifier)

        is UiNode.Badge -> NodeBadge(node, entities, item, modifier)

        // The click goes on the content's own modifier, so the whole drawn area is the target; a button inside
        // consumes its own taps (the innermost clickable wins).
        is UiNode.Tappable -> {
            val description = node.contentDescription?.let { interpolate(it, entities, item) }
            val tag = node.tag?.let { bindItemTokens(it, item) }
            var mod = modifier.clickable(onClickLabel = description) { dispatch(node.onClick) }
            if (description != null) mod = mod.semantics { contentDescription = description }
            if (tag != null) mod = mod.testTag(tag)
            RenderUiNode(node.content, entities, onDispatch, mod, item)
        }

        is UiNode.Box -> NodeBox(node, entities, onDispatch, item, modifier)

        is UiNode.ChoiceChips -> NodeChoiceChips(node, modifier)

        is UiNode.MediaPicker -> NodeMediaPicker(node, entities, modifier)

        is UiNode.Sticky -> NodeSticky(node, entities, onDispatch, item, modifier)

        is UiNode.Progress -> {
            val size = (node.sizeDp ?: PROGRESS_DEFAULT_SIZE_DP).dp
            CircularProgressIndicator(
                modifier = modifier.size(size).semantics { contentDescription = "In progress" },
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                // A thin ring at any size: about an eighth of the diameter, at least 1.5 dp.
                strokeWidth = maxOf(1.5.dp, size / 8),
            )
        }



        is UiNode.Layout -> LayoutNodeView(node, entities, item, modifier)

        is UiNode.Switch -> androidx.compose.material3.Switch(
            checked = isTruthy(node.checked, item),
            onCheckedChange = { dispatch(node.onClick) },
            modifier = modifier.semantics {
                node.contentDescription?.let { contentDescription = interpolate(it, entities, item) }
            },
        )

        is UiNode.Unknown -> Text(
            "Unsupported component: ${node.type.ifEmpty { "<missing type>" }}",
            style = MaterialTheme.typography.labelSmall,
            color = Color.Gray,
            modifier = modifier.padding(4.dp),
        )
    }
}

// ---------------------------------------------------------------------------
// Generic nodes (DECISIONS D34): core://List, core://Image, core://TextField.
// ---------------------------------------------------------------------------

/**
 * The items of `core://List`: `queries[queryKey]` in order (de-duplicated, URIs without an entity skipped), each the
 * entity plus a synthetic `uri`; null while the query is absent (not loaded yet).
 */
fun listItems(queryKey: String, queries: Map<String, List<String>>, entities: Map<String, JsonObject>): List<JsonObject>? =
    queries[queryKey]?.distinct()?.mapNotNull { uri -> entities[uri]?.let { JsonObject(it + ("uri" to JsonPrimitive(uri))) } }

/** Start padding of a list item: `item[indent.field]` (a number) times `stepDp`, at most `max` (default 4) steps. */
fun indentDp(indent: Indent?, item: JsonObject): Int {
    if (indent == null) return 0
    val n = (item[indent.field] as? JsonPrimitive)?.takeIf { !it.isString }?.content?.toDoubleOrNull()?.toInt() ?: 0
    return n.coerceIn(0, indent.max ?: 4) * indent.stepDp.coerceAtLeast(0)
}

/** [s] lowercased with common Latin accents removed ("Crème Brûlée" -> "creme brulee"), for [matchesFilter]. */
fun foldForSearch(s: String): String {
    val out = StringBuilder(s.length)
    for (c in s.lowercase()) {
        if (c in '\u0300'..'\u036F') continue // combining marks (decomposed accents)
        out.append(ACCENT_FOLDS[c] ?: c)
    }
    return out.toString()
}

private val ACCENT_FOLDS: Map<Char, String> = buildMap {
    fun fold(chars: String, to: String) = chars.forEach { put(it, to) }
    fold("àáâãäåāăą", "a"); fold("çćĉċč", "c"); fold("ďđ", "d"); fold("èéêëēĕėęě", "e"); fold("ĝğġģ", "g")
    fold("ĥħ", "h"); fold("ìíîïĩīĭįı", "i"); fold("ĵ", "j"); fold("ķ", "k"); fold("ĺļľŀł", "l"); fold("ñńņňŉ", "n")
    fold("òóôõöøōŏő", "o"); fold("ŕŗř", "r"); fold("śŝşš", "s"); fold("ţťŧ", "t"); fold("ùúûüũūŭůűų", "u")
    fold("ŵ", "w"); fold("ýÿŷ", "y"); fold("źżž", "z"); fold("ß", "ss"); fold("æ", "ae"); fold("œ", "oe")
}

/** The query of [filter]: the form value for `$form.<field>` (blank when unset), else the literal text. */
fun filterQuery(filter: ListFilter, form: Map<String, String>?): String = formOrLiteral(filter.value, form)

/** An item field as searchable text: a string or number, or the strings of an array, joined by spaces. */
private fun searchableText(v: JsonElement?): String = when (v) {
    is JsonPrimitive -> if (v is JsonNull) "" else v.content
    is JsonArray -> v.joinToString(" ") { searchableText(it) }
    else -> ""
}

/**
 * Whether [item] passes a list filter (D43): a blank [query] matches everything; otherwise every word of it must be
 * found in at least one of [fields] (case and accents ignored; words may match different fields).
 */
fun matchesFilter(query: String, item: JsonObject, fields: List<String>): Boolean {
    val words = foldForSearch(query).split(Regex("\\s+")).filter { it.isNotEmpty() }
    if (words.isEmpty()) return true
    val haystacks = fields.map { foldForSearch(searchableText(item[it])) }
    return words.all { w -> haystacks.any { it.contains(w) } }
}

/** For each of [items], whether it starts a group: its [field] differs from the previous item's (D43). */
fun groupStarts(items: List<JsonObject>, field: String): List<Boolean> =
    items.mapIndexed { i, item -> i == 0 || item[field] != items[i - 1][field] }

/** For each of [items], the size of the group it is in ([groupStarts]): a group header's `group_count` (D44). */
fun groupCounts(starts: List<Boolean>): List<Int> {
    val out = MutableList(starts.size) { 0 }
    var begin = 0
    for (i in 1..starts.size) {
        if (i == starts.size || starts[i]) {
            for (j in begin until i) out[j] = i - begin
            begin = i
        }
    }
    return out
}

/**
 * Whether [item] passes a list's `where` (D44): a blank [value] matches everything; otherwise the item's [field] must
 * equal it exactly.
 */
fun matchesWhere(value: String, item: JsonObject, field: String): Boolean =
    value.isBlank() || searchableText(item[field]) == value

/** [where] with each value resolved ([formOrLiteral]): (field, value) pairs for [matchesAll]. */
fun whereConditions(where: List<ListWhere>, form: Map<String, String>?): List<Pair<String, String>> =
    where.map { it.field to formOrLiteral(it.equals, form) }

/** Whether [item] passes every condition of a list's or count's `where` ([matchesWhere] each). */
fun matchesAll(conditions: List<Pair<String, String>>, item: JsonObject): Boolean =
    conditions.all { (field, value) -> matchesWhere(value, item, field) }

/** A form-or-literal value: the form value for `$form.<field>` (blank when unset), else [value] itself. */
fun formOrLiteral(value: String, form: Map<String, String>?): String =
    if (value.startsWith("\$form.")) form?.get(value.removePrefix("\$form.")).orEmpty() else value

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ItemList(
    node: UiNode.ItemList,
    entities: Map<String, JsonObject>,
    onDispatch: (ActionIntent) -> Unit,
    modifier: Modifier,
) {
    val queries = LocalConstructQueries.current
    val all = remember(node.queryKey, queries, entities) { listItems(node.queryKey, queries, entities) }
    // Reading the form here recomposes the list as the search field is typed into.
    val form = LocalForm.current
    val query = node.filter?.let { filterQuery(it, form) }.orEmpty()
    val conditions = whereConditions(node.where, form)
    val items = remember(all, node.filter, query, conditions) {
        val f = node.filter
        all
            ?.let { list -> if (conditions.all { it.second.isBlank() }) list else list.filter { matchesAll(conditions, it) } }
            ?.let { list -> if (f == null || query.isBlank()) list else list.filter { matchesFilter(query, it, f.fields) } }
    }
    val starts = remember(items, node.groupBy) { node.groupBy?.let { g -> items?.let { groupStarts(it, g.field) } } }
    val counts = remember(starts) { starts?.let(::groupCounts) }
    val columns = node.columns.coerceAtLeast(1)
    Column(
        modifier.fillMaxWidth(),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(node.spacingDp.dp),
    ) {
        val message = when {
            items == null || items.isNotEmpty() -> null
            all.isNullOrEmpty() -> node.emptyText
            else -> node.filter?.noMatchText ?: node.emptyText
        }
        if (message != null) {
            Text(
                message,
                style = MaterialTheme.typography.bodyMedium,
                color = Color.Gray,
                modifier = Modifier.padding(16.dp).testTag("list-empty"),
            )
        }
        if (columns > 1 && items != null) {
            ItemGrid(node, columns, items, starts, counts, entities, onDispatch)
            return@Column
        }
        val focus = node.focus
        val focused = focusIndex(items, focus)
        val focusRequester = remember(focus?.uri) { BringIntoViewRequester() }
        // Scroll to the focused item once per screen (rememberSaveable: not again after Back or a rotation).
        var focusScrolled by rememberSaveable(focus?.uri) { mutableStateOf(false) }
        val density = LocalDensity.current
        var focusHeight by remember(focus?.uri) { mutableStateOf(0) }
        LaunchedEffect(focus?.uri, focused >= 0) {
            if (focused < 0 || focusScrolled) return@LaunchedEffect
            withFrameNanos { } // laid out first
            // A little of the thread above and below stays in view.
            val margin = with(density) { FOCUS_MARGIN.toPx() }
            focusRequester.bringIntoView(Rect(0f, -margin, 1f, focusHeight + margin))
            focusScrolled = true
        }
        items?.forEachIndexed { i, item ->
            val header = node.groupBy?.header
            if (header != null && starts?.getOrNull(i) == true) {
                // Keyed by the item it heads: headers hold no state, and move with the filter.
                key("group-header", item["uri"]) {
                    val headed = JsonObject(item + ("group_count" to JsonPrimitive(counts?.getOrNull(i) ?: 0)))
                    RenderUiNode(header, entities, onDispatch, Modifier.widthIn(max = FEED_MAX_WIDTH).fillMaxWidth(), headed)
                }
            }
            // Keyed by entity URI, so per-item state follows the item when the list changes.
            key(item["uri"]) {
                // Phone: full width. Wider screens: a centered, readable column.
                var mod = Modifier.widthIn(max = FEED_MAX_WIDTH).fillMaxWidth()
                val fg = if (i == focused) parseHexColor(focus?.contentColor) else null
                if (i == focused) {
                    mod = mod.bringIntoViewRequester(focusRequester).onSizeChanged { focusHeight = it.height }.testTag("list-focus")
                    parseHexColor(focus?.background)?.let { mod = mod.background(it) }
                }
                mod = mod.padding(horizontal = node.paddingHDp.dp).padding(start = indentDp(node.indent, item).dp)
                if (fg != null) {
                    CompositionLocalProvider(LocalContentColor provides fg) {
                        RenderUiNode(node.itemTemplate, entities, onDispatch, mod, item)
                    }
                } else {
                    RenderUiNode(node.itemTemplate, entities, onDispatch, mod, item)
                }
            }
        }
    }
}

/** Room kept above and below a list's focused item when it is scrolled into view. */
private val FOCUS_MARGIN = 96.dp

/** The index in [items] of the item [focus] names (its entity URI), or -1 (no focus, not loaded, or not listed). */
fun focusIndex(items: List<JsonObject>?, focus: ListFocus?): Int {
    if (focus == null || items == null) return -1
    return items.indexOfFirst { (it["uri"] as? JsonPrimitive)?.content == focus.uri }
}

/**
 * The rows of a [columns]-column grid over [count] items (D48): item indexes, at most [columns] per row; an item that
 * starts a group ([starts]) starts a new row, so its header can span the width above it.
 */
fun gridRows(count: Int, starts: List<Boolean>?, columns: Int): List<List<Int>> {
    val rows = mutableListOf<MutableList<Int>>()
    for (i in 0 until count) {
        if (rows.isEmpty() || rows.last().size >= columns || starts?.getOrNull(i) == true) rows.add(mutableListOf())
        rows.last().add(i)
    }
    return rows
}

/** A `core://List` with `columns` > 1 (D48): rows of equal cells, top-aligned, a short last row left-aligned. */
@Composable
private fun ItemGrid(
    node: UiNode.ItemList,
    columns: Int,
    items: List<JsonObject>,
    starts: List<Boolean>?,
    counts: List<Int>?,
    entities: Map<String, JsonObject>,
    onDispatch: (ActionIntent) -> Unit,
) {
    val rows = remember(items.size, starts, columns) { gridRows(items.size, starts, columns) }
    val gap = node.spacingDp.dp
    rows.forEach { row ->
        val first = items[row.first()]
        val header = node.groupBy?.header
        if (header != null && starts?.getOrNull(row.first()) == true) {
            key("group-header", first["uri"]) {
                val headed = JsonObject(first + ("group_count" to JsonPrimitive(counts?.getOrNull(row.first()) ?: 0)))
                RenderUiNode(header, entities, onDispatch, Modifier.widthIn(max = FEED_MAX_WIDTH).fillMaxWidth(), headed)
            }
        }
        // Keyed by the row's first entity; cells by their own URI.
        key("grid-row", first["uri"]) {
            Row(
                Modifier.widthIn(max = FEED_MAX_WIDTH).fillMaxWidth().padding(horizontal = node.paddingHDp.dp),
                horizontalArrangement = Arrangement.spacedBy(gap),
                verticalAlignment = Alignment.Top,
            ) {
                row.forEach { i ->
                    key(items[i]["uri"]) {
                        RenderUiNode(node.itemTemplate, entities, onDispatch, Modifier.weight(1f), items[i])
                    }
                }
                repeat(columns - row.size) { Spacer(Modifier.weight(1f)) }
            }
        }
    }
}

/**
 * A text field bound to the form value [UiNode.TextField.field]; Send dispatches `on_submit` and clears it. Without
 * `on_submit` it is one line with a Clear button (a search box), or with `lines` (schema 9) a field of up to that many
 * lines (a caption).
 */
@Composable
private fun FormTextField(node: UiNode.TextField, dispatch: (ActionIntent) -> Unit, modifier: Modifier) {
    val form = LocalForm.current
    val value = form?.get(node.field).orEmpty()
    val submit = node.onSubmit
    val lines = node.lines?.coerceIn(1, MAX_TEXT_FIELD_LINES)
    val singleLine = submit == null && (lines == null || lines == 1)
    Row(modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        OutlinedTextField(
            value = value,
            onValueChange = { v -> form?.set(node.field, node.maxLength?.let { v.take(it) } ?: v) },
            placeholder = { Text(node.placeholder) },
            minLines = if (submit == null && lines != null) minOf(lines, 3) else 1,
            maxLines = if (singleLine) 1 else lines ?: 4,
            singleLine = singleLine,
            trailingIcon = if (singleLine && value.isNotEmpty()) {
                {
                    IconButton(
                        onClick = { form?.remove(node.field) },
                        modifier = Modifier.testTag("textfield-clear:${node.field}"),
                    ) {
                        Icon(ConstructIcons.Close, contentDescription = "Clear")
                    }
                }
            } else {
                null
            },
            modifier = Modifier.weight(1f).testTag("textfield:${node.field}"),
        )
        if (submit != null) {
            IconButton(
                onClick = {
                    if (value.isNotBlank()) {
                        dispatch(submit)
                        form?.remove(node.field)
                    }
                },
                enabled = value.isNotBlank(),
                modifier = Modifier.testTag("textfield-send:${node.field}"),
            ) {
                Icon(ConstructIcons.Send, contentDescription = "Send")
            }
        }
    }
}
