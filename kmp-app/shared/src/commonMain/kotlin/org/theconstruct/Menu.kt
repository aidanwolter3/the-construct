package org.theconstruct

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DrawerValue
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalDrawerSheet
import androidx.compose.material3.ModalNavigationDrawer
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberDrawerState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

// The hamburger menu: a small button over the top-left corner of the layout (badged when offline, when mutations wait
// to be sent, or when a pinned version has a newer one) opens a Material3 modal drawer with the sections Apps (each
// app's version history opens the version picker), Connection, Account and Server. Shared Compose: desktop, Android
// and the simulator run the same code. Test tags are listed in README "Simulation mode".

/** Start padding the workspace title gets so it sits beside the menu button (8 dp margin + 40 dp button + 8 dp). */
internal val MENU_BUTTON_INSET = 56.dp

/** An apps / history load. */
sealed class Load<out T> {
    object Loading : Load<Nothing>()
    data class Ready<T>(val value: T) : Load<T>()
    data class Failed(val message: String) : Load<Nothing>()
}

/**
 * The menu's state, owned by [ConstructController] so an open drawer, the version picker and a half-typed server URL
 * survive rotation. Loads run in the drawer's effects; a version selection runs to completion even if the composition
 * goes away meanwhile.
 */
@Stable
class MenuState internal constructor(private val controller: ConstructController) {
    var open by mutableStateOf(false)
        private set

    /** The app whose version picker is shown in the drawer, or null for the sections. */
    var historyAppId by mutableStateOf<String?>(null)
        private set

    var apps: Load<List<AppSummary>> by mutableStateOf(Load.Loading)
        private set

    /** Bumped on every open and by Retry: the apps load is keyed on it. */
    var appsToken by mutableStateOf(0)
        private set

    val history = mutableStateMapOf<String, VersionHistory>()

    /** Bumped when the picker opens and by Try again: the history load is keyed on it. */
    var historyToken by mutableStateOf(0)
        private set

    /** The version being fetched for display (its row shows a spinner). */
    var selecting by mutableStateOf<Long?>(null)
        private set
    var selectError by mutableStateOf<String?>(null)
        private set

    /** An app row that could not be opened. */
    var appError by mutableStateOf<String?>(null)
        private set

    var serverExpanded by mutableStateOf(false)

    /** The custom server URL being typed, or null when that field is closed. */
    var customUrl by mutableStateOf<String?>(null)

    fun openMenu() {
        open = true
        appError = null
        appsToken++
    }

    fun closeMenu() {
        open = false
        historyAppId = null
        selectError = null
        appError = null
        serverExpanded = false
        customUrl = null
    }

    /** Opens the version picker of [appId] (also opening the drawer). */
    fun showHistory(appId: String) {
        if (!open) openMenu()
        historyAppId = appId
        selectError = null
        historyToken++
    }

    /** Back closes the innermost level: the custom URL field, the version picker, then the drawer. */
    fun back() {
        when {
            customUrl != null -> customUrl = null
            historyAppId != null -> {
                historyAppId = null
                selectError = null
            }
            else -> closeMenu()
        }
    }

    fun retryApps() {
        appsToken++
    }

    /**
     * The last apps reload failed while an older list is still shown. Not set when it failed because the pod can't be
     * reached: Connection already says "Offline", and the list reloads once back online ([appsFailedOffline]).
     */
    var appsStaleError by mutableStateOf<String?>(null)
        private set

    /** The last apps load failed because the pod was unreachable: the drawer reloads when the engine is back online. */
    var appsFailedOffline by mutableStateOf(false)
        private set

    /** (Re)loads the apps; a failed reload keeps the last list (with the error under it unless offline). */
    internal suspend fun loadApps() {
        if (apps !is Load.Ready) apps = Load.Loading
        try {
            apps = Load.Ready(controller.apps.apps())
            appsStaleError = null
            appsFailedOffline = false
        } catch (e: LayoutApiException) {
            val offline = e.kind == LayoutApiException.Kind.Offline
            appsFailedOffline = offline
            when {
                apps !is Load.Ready -> apps = Load.Failed(e.message)
                offline -> appsStaleError = null
                else -> appsStaleError = e.message
            }
        }
    }

    /** Loads the newest page of [appId]'s history (replacing what was loaded, keeping it on screen meanwhile). */
    internal suspend fun loadHistory(appId: String) {
        val cur = history[appId] ?: VersionHistory()
        history[appId] = cur.copy(error = null)
        history[appId] = try {
            cur.withFirstPage(controller.apps.versions(appId))
        } catch (e: LayoutApiException) {
            cur.copy(error = e.message, loadingMore = false)
        }
    }

    fun retryHistory(appId: String) {
        history.remove(appId)
        historyToken++
    }

    /** "Load older": appends the next page. */
    fun loadOlder(appId: String, scope: CoroutineScope) {
        val cur = history[appId] ?: return
        val before = cur.nextBefore ?: return
        if (cur.loadingMore) return
        history[appId] = cur.copy(loadingMore = true, error = null)
        scope.launch {
            try {
                val page = controller.apps.versions(appId, before = before)
                history[appId]?.let { history[appId] = it.withOlderPage(before, page) }
            } catch (e: LayoutApiException) {
                history[appId]?.let { history[appId] = it.copy(loadingMore = false, error = e.message) }
            } finally {
                history[appId]?.let { if (it.loadingMore) history[appId] = it.copy(loadingMore = false) }
            }
        }
    }

    /** Opens [appId] (following its latest version) and closes the drawer; an error stays in the drawer. */
    fun openApp(appId: String) {
        if (controller.openApp(appId)) {
            closeMenu()
        } else {
            appError = OPEN_APP_UNAVAILABLE
        }
    }

    /** "Latest": follow [appId]'s newest version again (opening the app first if needed) and close the drawer. */
    fun selectLatest(appId: String) {
        if (!controller.openApp(appId)) {
            selectError = OPEN_APP_UNAVAILABLE
            return
        }
        controller.followLatest()
        closeMenu()
    }

    /**
     * Shows [version] of [appId]: fetches its spec (cached), opens the app if it is another one, pins it and closes the
     * drawer. Client-local: nothing changes on the pod. A failure stays in the picker.
     */
    fun selectVersion(appId: String, version: Long, scope: CoroutineScope) {
        if (selecting != null) return
        selecting = version
        selectError = null
        scope.launch(start = CoroutineStart.UNDISPATCHED) {
            withContext(NonCancellable) {
                try {
                    val spec = controller.apps.spec(appId, version)
                    when {
                        // The UI went away meanwhile (desktop server switch, Android onCleared): change nothing.
                        controller.isClosed -> Unit
                        controller.openApp(appId) && controller.pin(appId, version, spec) -> closeMenu()
                        !controller.isClosed -> selectError = OPEN_APP_UNAVAILABLE
                    }
                } catch (e: LayoutApiException) {
                    selectError = e.message
                } finally {
                    selecting = null
                }
            }
        }
    }

    companion object {
        const val OPEN_APP_UNAVAILABLE = "Opening other apps isn't available here yet."
    }
}

/** The hamburger button (`menu-button`, "Menu") with its badge dot (`menu-badge`). */
@Composable
internal fun MenuButton(badge: Boolean, onClick: () -> Unit, modifier: Modifier = Modifier) {
    Box(modifier) {
        Surface(
            shape = CircleShape,
            color = MaterialTheme.colorScheme.surface.copy(alpha = 0.92f),
            shadowElevation = 2.dp,
            modifier = Modifier.size(40.dp),
        ) {
            IconButton(onClick = onClick, modifier = Modifier.testTag("menu-button")) {
                Icon(ConstructIcons.Menu, contentDescription = "Menu", modifier = Modifier.size(22.dp))
            }
        }
        if (badge) {
            Box(
                Modifier.align(Alignment.TopEnd).offset(x = 1.dp, y = (-1).dp).size(11.dp)
                    .background(MaterialTheme.colorScheme.error, CircleShape)
                    .border(2.dp, Color.White, CircleShape)
                    .testTag("menu-badge"),
            )
        }
    }
}

/**
 * The modal drawer around [content]. Its contents are composed only while it is open (or animating), so a closed menu
 * adds nothing to the screen's semantics. Swiping only closes it; the button opens it.
 */
@Composable
internal fun MenuDrawer(
    controller: ConstructController,
    state: ConstructState,
    pin: PinnedVersion?,
    server: ServerChoice?,
    content: @Composable () -> Unit,
) {
    val menu = controller.menu
    val drawerState = rememberDrawerState(if (menu.open) DrawerValue.Open else DrawerValue.Closed)
    LaunchedEffect(menu.open) {
        if (menu.open) drawerState.open() else drawerState.close()
    }
    // A scrim tap or a swipe closes the drawer itself: mirror that into the menu state once it has settled closed.
    LaunchedEffect(drawerState) {
        var wasOpen = false
        snapshotFlow { drawerState.currentValue to drawerState.targetValue }.collect { (current, target) ->
            if (current == DrawerValue.Open) wasOpen = true
            if (wasOpen && current == DrawerValue.Closed && target == DrawerValue.Closed) {
                wasOpen = false
                if (menu.open) menu.closeMenu()
            }
        }
    }
    PlatformBackHandler(enabled = menu.open) { menu.back() }
    val shown = drawerState.currentValue == DrawerValue.Open || drawerState.targetValue == DrawerValue.Open ||
        drawerState.isAnimationRunning
    ModalNavigationDrawer(
        drawerState = drawerState,
        gesturesEnabled = drawerState.isOpen,
        drawerContent = {
            ModalDrawerSheet(Modifier.width(304.dp)) {
                if (shown) {
                    val historyId = menu.historyAppId
                    if (historyId != null) {
                        VersionPicker(controller, state, pin, historyId)
                    } else {
                        MenuSections(controller, state, pin, server)
                    }
                }
            }
        },
        content = content,
    )
}

@Composable
private fun MenuSections(controller: ConstructController, state: ConstructState, pin: PinnedVersion?, server: ServerChoice?) {
    val menu = controller.menu
    LaunchedEffect(menu.appsToken) { menu.loadApps() }
    // Back online after a load that couldn't reach the pod: refresh the list.
    LaunchedEffect(state.isOnline) {
        if (state.isOnline == true && menu.appsFailedOffline) menu.retryApps()
    }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).testTag("menu-drawer").padding(bottom = 16.dp)) {
        Row(Modifier.fillMaxWidth().padding(start = 20.dp, end = 8.dp, top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(
                "The Construct",
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f).semantics { heading() },
            )
            IconButton(onClick = menu::closeMenu, modifier = Modifier.testTag("menu-close")) {
                Icon(ConstructIcons.Close, contentDescription = "Close menu", modifier = Modifier.size(22.dp))
            }
        }
        if (newerVersionAvailable(state, pin)) NewerVersionBanner(controller, state, pin!!)

        SectionTitle("Apps")
        AppsSection(controller, state, pin)

        SectionDivider()
        SectionTitle("Connection")
        ConnectionSection(state)

        SectionDivider()
        SectionTitle("Account")
        AccountSection(controller, state)

        if (server != null) {
            SectionDivider()
            SectionTitle("Server")
            ServerSection(menu, server)
        }
    }
}

@Composable
private fun SectionTitle(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 20.dp, end = 20.dp, top = 12.dp, bottom = 4.dp).semantics { heading() },
    )
}

@Composable
private fun SectionDivider() {
    HorizontalDivider(Modifier.padding(horizontal = 20.dp, vertical = 4.dp), color = MaterialTheme.colorScheme.outlineVariant)
}

/** "Newer version available" (`menu-newer`) with a "Show latest" action (`menu-newer-latest`). */
@Composable
private fun NewerVersionBanner(controller: ConstructController, state: ConstructState, pin: PinnedVersion) {
    Surface(
        color = MaterialTheme.colorScheme.tertiaryContainer,
        shape = RoundedCornerShape(16.dp),
        modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp).testTag("menu-newer"),
    ) {
        Column(Modifier.padding(start = 16.dp, end = 8.dp, top = 12.dp, bottom = 4.dp)) {
            Text("Newer version available", style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold)
            Text(
                "You're viewing v${pin.version}, v${state.activeLayout.version} is the latest.",
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.padding(top = 2.dp),
            )
            TextButton(
                onClick = { controller.menu.selectLatest(pin.appId) },
                modifier = Modifier.align(Alignment.End).testTag("menu-newer-latest"),
            ) { Text("Show latest") }
        }
    }
}

private fun appIcon(icon: String): ImageVector = when (icon) {
    "photo" -> ConstructIcons.Photo
    "counter" -> ConstructIcons.PlusOne
    else -> ConstructIcons.Dashboard
}

@Composable
private fun AppsSection(controller: ConstructController, state: ConstructState, pin: PinnedVersion?) {
    val menu = controller.menu
    val appIdFlow by controller.appId.collectAsState()
    val current = appIdFlow ?: state.activeLayout.appId
    val loaded = (menu.apps as? Load.Ready)?.value.orEmpty()
    // The app on screen is always listed (e.g. a hidden dev app, or while the list can't load).
    val apps = if (current == null || loaded.any { it.id == current }) {
        loaded
    } else {
        listOf(AppSummary(current, (state.workspace as? UiNode.Workspace)?.title ?: current, latestVersion = state.activeLayout.version)) + loaded
    }
    apps.forEach { app ->
        val isCurrent = app.id == current
        val pinnedHere = pinnedInPicker(state, pin, app.id, isCurrent)
        val latest = (if (isCurrent) state.activeLayout.version else null) ?: app.latestVersion
        val subtitle = when {
            pinnedHere != null -> "Showing v${pinnedHere.version}" + (latest?.let { " · latest v$it" } ?: "")
            isCurrent -> "Latest" + (latest?.let { " · v$it" } ?: "")
            else -> latest?.let { "v$it" } ?: ""
        }
        AppRow(
            app, subtitle, isCurrent,
            enabled = isCurrent || controller.canOpenApps,
            onOpen = { if (isCurrent) menu.closeMenu() else menu.openApp(app.id) },
            onHistory = { menu.showHistory(app.id) },
        )
    }
    when (val a = menu.apps) {
        Load.Loading -> if (loaded.isEmpty()) LoadingRow("Loading apps…")
        is Load.Failed -> InlineError(a.message, tag = "apps-error", onRetry = menu::retryApps)
        is Load.Ready -> menu.appsStaleError?.let { InlineError(it, tag = "apps-error", onRetry = menu::retryApps) }
    }
    menu.appError?.let { InlineError(it, tag = "app-error", onRetry = null) }
}

@Composable
private fun AppRow(
    app: AppSummary,
    subtitle: String,
    selected: Boolean,
    enabled: Boolean,
    onOpen: () -> Unit,
    onHistory: () -> Unit,
) {
    val shape = RoundedCornerShape(28.dp)
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 2.dp).heightIn(min = 56.dp).clip(shape)
            .background(if (selected) MaterialTheme.colorScheme.secondaryContainer else Color.Transparent, shape)
            .clickable(enabled = enabled, onClick = onOpen)
            .testTag("app-${app.id}")
            .padding(start = 16.dp, end = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(
            appIcon(app.icon),
            contentDescription = null,
            tint = if (enabled) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.outline,
            modifier = Modifier.size(22.dp),
        )
        Spacer(Modifier.width(14.dp))
        Column(Modifier.weight(1f).padding(vertical = 8.dp)) {
            Text(
                app.title,
                style = MaterialTheme.typography.bodyLarge,
                fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
                color = if (enabled) Color.Unspecified else MaterialTheme.colorScheme.outline,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (subtitle.isNotEmpty()) {
                Text(subtitle, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1)
            }
        }
        IconButton(onClick = onHistory, modifier = Modifier.testTag("app-history-${app.id}")) {
            Icon(ConstructIcons.History, contentDescription = "Version history of ${app.title}", modifier = Modifier.size(22.dp))
        }
    }
}

@Composable
private fun ConnectionSection(state: ConstructState) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 6.dp).testTag("menu-connection")) {
        val online = state.isOnline
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(Modifier.size(10.dp).background(if (online == true) Color(0xFF2E7D32) else Color(0xFF9E9E9E), CircleShape))
            Spacer(Modifier.width(10.dp))
            Text(
                when (online) {
                    true -> "Online"
                    false -> "Offline"
                    null -> "Connecting…"
                },
                style = MaterialTheme.typography.bodyLarge,
            )
            if (state.pendingActions > 0) {
                Spacer(Modifier.width(16.dp))
                Box(Modifier.size(10.dp).background(Color(0xFFF9A825), CircleShape))
                Spacer(Modifier.width(8.dp))
                Text("${state.pendingActions} pending", style = MaterialTheme.typography.bodyLarge)
            }
        }
        if (online == false) {
            Text(
                "Changes are saved and sent when you're back online.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 2.dp),
            )
        }
        state.error?.let {
            Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(top = 4.dp))
        }
    }
}

@Composable
private fun AccountSection(controller: ConstructController, state: ConstructState) {
    val auth = state.auth
    Column(Modifier.fillMaxWidth().padding(start = 20.dp, end = 12.dp, top = 2.dp).testTag("menu-account")) {
        val signIn = findSignInIntent(state.workspace)
        when (auth.state) {
            "signed_in" -> Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "Signed in as " + (auth.username?.let { "@$it" } ?: "your account"),
                    style = MaterialTheme.typography.bodyLarge,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = controller::logout, modifier = Modifier.testTag("menu-sign-out")) { Text("Sign out") }
            }
            "pending" -> Text(
                auth.message ?: "Signing in…",
                style = MaterialTheme.typography.bodyLarge,
                modifier = Modifier.padding(vertical = 10.dp),
            )
            else -> Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    if (auth.state == "error") "Sign-in failed" + (auth.error?.let { ": $it" } ?: "") else "Not signed in",
                    style = MaterialTheme.typography.bodyLarge,
                    color = if (auth.state == "error") MaterialTheme.colorScheme.error else Color.Unspecified,
                    maxLines = 3,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f).padding(vertical = 10.dp),
                )
                if (signIn != null) {
                    TextButton(
                        onClick = {
                            controller.menu.closeMenu()
                            controller.dispatch(signIn)
                        },
                        modifier = Modifier.testTag("menu-sign-in"),
                    ) { Text("Sign in") }
                }
            }
        }
    }
}

@Composable
private fun ServerSection(menu: MenuState, server: ServerChoice) {
    Column(Modifier.fillMaxWidth()) {
        Row(
            Modifier.fillMaxWidth().clickable { menu.serverExpanded = !menu.serverExpanded }.testTag("menu-server")
                .padding(start = 20.dp, end = 16.dp, top = 6.dp, bottom = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text(server.label(), style = MaterialTheme.typography.bodyLarge)
                Text(
                    server.currentUrl,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Icon(
                if (menu.serverExpanded) ConstructIcons.ExpandLess else ConstructIcons.ExpandMore,
                contentDescription = if (menu.serverExpanded) "Hide servers" else "Choose a server",
            )
        }
        if (!menu.serverExpanded) return@Column
        server.options.forEach { option ->
            ChoiceRow(
                title = option.label,
                subtitle = option.url,
                checked = option.url.trimEnd('/') == server.currentUrl.trimEnd('/'),
                tag = "server-${option.key}",
                onClick = {
                    menu.serverExpanded = false
                    menu.customUrl = null
                    server.onSelect(option.url)
                },
            )
        }
        val custom = menu.customUrl
        val isCustom = server.options.none { it.url.trimEnd('/') == server.currentUrl.trimEnd('/') }
        ChoiceRow(
            title = "Custom URL…",
            subtitle = if (isCustom) server.currentUrl else if (server.cleartextHosts == null) ServerUrls.FORMAT_HINT else "https://host[:port]",
            checked = isCustom,
            tag = "server-custom",
            onClick = { menu.customUrl = if (isCustom) server.currentUrl else if (server.cleartextHosts == null) "http://" else "https://" },
        )
        if (custom != null) {
            val check = ServerUrls.check(custom, server.cleartextHosts)
            val url = (check as? ServerUrlCheck.Ok)?.url
            Column(Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 4.dp)) {
                OutlinedTextField(
                    value = custom,
                    onValueChange = { menu.customUrl = it },
                    singleLine = true,
                    isError = url == null,
                    label = { Text("Server URL") },
                    supportingText = {
                        when (check) {
                            is ServerUrlCheck.Ok -> Unit
                            ServerUrlCheck.Invalid -> Text(ServerUrls.FORMAT_HINT)
                            ServerUrlCheck.NeedsHttps -> Text(ServerUrls.NEEDS_HTTPS_HINT, Modifier.testTag("server-custom-hint"))
                        }
                    },
                    modifier = Modifier.fillMaxWidth().testTag("server-custom-url"),
                )
                Row(Modifier.align(Alignment.End)) {
                    TextButton(onClick = { menu.customUrl = null }) { Text("Cancel") }
                    TextButton(
                        enabled = url != null,
                        onClick = {
                            if (url != null) {
                                menu.customUrl = null
                                menu.serverExpanded = false
                                server.onSelect(url)
                            }
                        },
                        modifier = Modifier.testTag("server-custom-use"),
                    ) { Text("Use") }
                }
            }
        }
    }
}

/** A selectable row with a check mark when [checked] (server presets, versions). */
@Composable
private fun ChoiceRow(
    title: String,
    subtitle: String?,
    checked: Boolean,
    tag: String,
    onClick: () -> Unit,
    detail: String? = null,
    busy: Boolean = false,
    bold: Boolean = false,
) {
    Row(
        Modifier.fillMaxWidth().clickable(onClick = onClick).testTag(tag).padding(start = 20.dp, end = 16.dp, top = 8.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(title, style = MaterialTheme.typography.bodyLarge, fontWeight = if (bold || checked) FontWeight.SemiBold else FontWeight.Normal)
                if (!subtitle.isNullOrEmpty()) {
                    Text(
                        "  ·  $subtitle",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            if (!detail.isNullOrEmpty()) {
                Text(
                    detail,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        when {
            busy -> CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
            checked -> Icon(ConstructIcons.Check, contentDescription = "Selected", tint = MaterialTheme.colorScheme.primary)
        }
    }
}

/**
 * The version picker (`version-picker`) of [appId]: "Latest" (`version-latest`), then the history newest first
 * (`version-<n>`: "vN", relative time, note or source), paged with "Load older" (`versions-more`). The version on
 * screen is checked; when it is pinned to a version on an older page, a checked "Pinned: vN" row (`version-pinned`)
 * sits under "Latest" until that page is loaded.
 */
@Composable
private fun VersionPicker(controller: ConstructController, state: ConstructState, pin: PinnedVersion?, appId: String) {
    val menu = controller.menu
    val scope = rememberCoroutineScope()
    val appIdFlow by controller.appId.collectAsState()
    val isCurrent = appId == (appIdFlow ?: state.activeLayout.appId)
    // Reloaded when the picker opens, on Try again, and when a new version of the app on screen arrives.
    val latestOnScreen = if (isCurrent) state.activeLayout.version else null
    LaunchedEffect(appId, menu.historyToken, latestOnScreen) { menu.loadHistory(appId) }
    val title = (menu.apps as? Load.Ready)?.value?.firstOrNull { it.id == appId }?.title
        ?: if (isCurrent) (state.workspace as? UiNode.Workspace)?.title ?: appId else appId
    val h = menu.history[appId] ?: VersionHistory()
    val pinnedHere = pinnedInPicker(state, pin, appId, isCurrent)
    val now = nowEpochSeconds()
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).testTag("version-picker").padding(bottom = 16.dp)) {
        Row(Modifier.fillMaxWidth().padding(start = 4.dp, end = 8.dp, top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = menu::back, modifier = Modifier.testTag("menu-back")) {
                Icon(ConstructIcons.ArrowBack, contentDescription = "Back", modifier = Modifier.size(22.dp))
            }
            Column(Modifier.weight(1f).padding(start = 4.dp)) {
                Text("Versions", style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold, modifier = Modifier.semantics { heading() })
                Text(title, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        menu.selectError?.let { InlineError(it, tag = "version-error", onRetry = null) }
        val latest = latestOnScreen ?: h.versions.firstOrNull()?.version
        ChoiceRow(
            title = "Latest",
            subtitle = latest?.let { "v$it" },
            detail = "Follows new versions as they're published",
            checked = isCurrent && pinnedHere == null,
            tag = "version-latest",
            onClick = { menu.selectLatest(appId) },
            bold = true,
        )
        pinnedRowVersion(pinnedHere, h)?.let { v ->
            ChoiceRow(
                title = "Pinned: v$v",
                subtitle = null,
                detail = "The version on screen (older versions are below \"Load older\")",
                checked = true,
                busy = menu.selecting == v,
                tag = "version-pinned",
                onClick = { menu.selectVersion(appId, v, scope) },
            )
        }
        HorizontalDivider(Modifier.padding(horizontal = 20.dp, vertical = 4.dp), color = MaterialTheme.colorScheme.outlineVariant)
        h.versions.forEach { v ->
            ChoiceRow(
                title = "v${v.version}",
                subtitle = versionTimeLabel(v.createdAt, now),
                detail = versionDetail(v),
                checked = pinnedHere?.version == v.version,
                busy = menu.selecting == v.version,
                tag = "version-${v.version}",
                onClick = { menu.selectVersion(appId, v.version, scope) },
            )
        }
        when {
            h.error != null -> InlineError(h.error, tag = "versions-error", onRetry = { menu.retryHistory(appId) })
            !h.loaded -> LoadingRow("Loading versions…")
            h.loaded && h.versions.isEmpty() -> Text(
                "No versions yet.",
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.padding(horizontal = 20.dp, vertical = 12.dp),
            )
        }
        if (h.hasMore) {
            TextButton(
                onClick = { menu.loadOlder(appId, scope) },
                enabled = !h.loadingMore,
                modifier = Modifier.padding(horizontal = 12.dp).testTag("versions-more"),
            ) {
                if (h.loadingMore) {
                    CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                    Spacer(Modifier.width(8.dp))
                }
                Text("Load older")
            }
        }
    }
}

@Composable
private fun LoadingRow(text: String) {
    Row(Modifier.padding(horizontal = 20.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
        CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        Spacer(Modifier.width(12.dp))
        Text(text, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun InlineError(message: String, tag: String, onRetry: (() -> Unit)?) {
    Row(
        Modifier.fillMaxWidth().padding(start = 20.dp, end = 8.dp, top = 4.dp, bottom = 4.dp).testTag(tag),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(message, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error, modifier = Modifier.weight(1f))
        if (onRetry != null) TextButton(onClick = onRetry) { Text("Try again") }
    }
}
