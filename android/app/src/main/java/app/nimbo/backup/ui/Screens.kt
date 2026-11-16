package app.nimbo.backup.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Logout
import androidx.compose.material.icons.automirrored.filled.MenuBook
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.CloudUpload
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Error
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.PhotoLibrary
import androidx.compose.material.icons.filled.QrCodeScanner
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Snackbar
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import java.util.Locale

@Composable
fun NimboApp(
    state: UiState,
    browse: BrowseState,
    tab: MainTab,
    actions: AppActions,
) {
    val snackbarHostState = remember { SnackbarHostState() }
    LaunchedEffect(state.snackbar) {
        state.snackbar?.let {
            snackbarHostState.showSnackbar(it)
            actions.onSnackbarConsumed()
        }
    }

    val filesCanGoUp = tab == MainTab.Files && browse.files.folderId.isNotBlank()
    BackHandler(
        enabled = browse.docViewer != null ||
            browse.imageViewer != null ||
            filesCanGoUp ||
            tab != MainTab.Backups,
    ) {
        when {
            browse.docViewer != null -> actions.onCloseDoc()
            browse.imageViewer != null -> actions.onCloseImageViewer()
            filesCanGoUp -> actions.onGoToParent()
            tab != MainTab.Backups -> actions.onSelectTab(MainTab.Backups)
        }
    }

    if (state.loading) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            CircularProgressIndicator()
        }
        return
    }

    if (!state.configured) {
        Scaffold(
            snackbarHost = { SnackbarHost(snackbarHostState) { Snackbar(snackbarData = it) } },
        ) { padding ->
            SetupScreen(
                Modifier.padding(padding),
                initialUrl = state.serverUrl,
                initialDeviceName = state.deviceName.ifBlank { defaultDeviceName() },
                onSave = { url, token, device, insecure, test ->
                    actions.onTestAndSave(url, token, device, insecure, test)
                },
                onScanQr = { actions.onScanQr() },
            )
        }
        return
    }

    Box(Modifier.fillMaxSize()) {
        Scaffold(
            topBar = { NimboTopBar(state, browse, tab, actions) },
            bottomBar = {
                NavigationBar {
                    MainTab.entries.forEach { entry ->
                        NavigationBarItem(
                            selected = tab == entry,
                            onClick = { actions.onSelectTab(entry) },
                            icon = { Icon(tabIcon(entry), contentDescription = entry.label) },
                            label = { Text(entry.label) },
                        )
                    }
                }
            },
            floatingActionButton = {
                if (tab == MainTab.Backups) {
                    FloatingActionButton(onClick = { actions.onAddFolder() }) {
                        Icon(Icons.Default.Add, contentDescription = "Add folder")
                    }
                }
            },
            snackbarHost = { SnackbarHost(snackbarHostState) { Snackbar(snackbarData = it) } },
        ) { padding ->
            when (tab) {
                MainTab.Backups -> FolderScreen(Modifier.padding(padding), state, actions)
                MainTab.Files -> FilesScreen(
                    Modifier.padding(padding),
                    state,
                    browse.files,
                    browse.downloading,
                    actions,
                )
                MainTab.Gallery -> GalleryScreen(
                    Modifier.padding(padding),
                    browse.gallery,
                    browse.downloading,
                    actions,
                )
                MainTab.Search -> SearchScreen(Modifier.padding(padding), browse.search, actions)
                MainTab.Vault -> VaultScreen(Modifier.padding(padding), state, browse.vault, actions)
            }
        }

        browse.docViewer?.let { viewer ->
            DocViewerOverlay(viewer, actions)
        }
        browse.imageViewer?.let { item ->
            MediaViewerOverlay(item, browse.downloading, actions)
        }
    }
}

class AppActions(
    val onTestAndSave: (url: String, token: String, deviceName: String, allowInsecureHttp: Boolean, testNow: Boolean) -> Unit,
    val onAddFolder: () -> Unit,
    val onToggleFolder: (id: Long, enabled: Boolean) -> Unit,
    val onRemoveFolder: (id: Long) -> Unit,
    val onRetryFailed: (id: Long) -> Unit,
    val onToggleInstant: (enabled: Boolean) -> Unit,
    val onBackupNow: () -> Unit,
    val onDisconnect: () -> Unit,
    val onDismissObserverError: () -> Unit,
    val onSnackbarConsumed: () -> Unit,
    val onScanQr: () -> Unit,
    val onSelectTab: (MainTab) -> Unit,
    val onRefreshFiles: () -> Unit,
    val onOpenFolder: (String) -> Unit,
    val onGoToParent: () -> Unit,
    val onOpenItem: (app.nimbo.backup.data.api.NodeSummary) -> Unit,
    val onDownload: (app.nimbo.backup.data.api.NodeSummary) -> Unit,
    val onSetMediaFilter: (MediaFilter) -> Unit,
    val onLoadGallery: (Boolean) -> Unit,
    val onSearch: (String) -> Unit,
    val onLoadVault: (Boolean) -> Unit,
    val onOpenDoc: (app.nimbo.backup.data.api.DocSummary) -> Unit,
    val onCloseDoc: () -> Unit,
    val onCloseImageViewer: () -> Unit,
    val onThumbnail: suspend (app.nimbo.backup.data.api.NodeSummary) -> ByteArray?,
    val onOpenImage: suspend (app.nimbo.backup.data.api.NodeSummary) -> ByteArray?,
)

private fun tabIcon(tab: MainTab) = when (tab) {
    MainTab.Backups -> Icons.Default.CloudUpload
    MainTab.Files -> Icons.Default.Folder
    MainTab.Gallery -> Icons.Default.PhotoLibrary
    MainTab.Search -> Icons.Default.Search
    MainTab.Vault -> Icons.AutoMirrored.Filled.MenuBook
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun NimboTopBar(
    state: UiState,
    browse: BrowseState,
    tab: MainTab,
    actions: AppActions,
) {
    val title = when (tab) {
        MainTab.Backups -> "Nimbo"
        MainTab.Files -> browse.files.folderName
        MainTab.Gallery -> "Gallery"
        MainTab.Search -> "Search"
        MainTab.Vault -> "Vault"
    }
    val subtitle = when {
        tab == MainTab.Backups -> state.serverUrl.substringAfter("://")
        tab == MainTab.Files && browse.files.breadcrumbs.isNotEmpty() ->
            browse.files.breadcrumbs.joinToString(" / ") { it.name }
        tab == MainTab.Vault && state.backupScope == true -> "Read-only · backup token"
        tab == MainTab.Vault -> "Read-only"
        else -> ""
    }
    TopAppBar(
        title = {
            Column {
                Text(title)
                if (subtitle.isNotBlank()) {
                    Text(
                        subtitle,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                    )
                }
            }
        },
        actions = {
            when (tab) {
                MainTab.Backups -> {
                    IconButton(onClick = { actions.onBackupNow() }) {
                        Icon(Icons.Default.Refresh, contentDescription = "Backup now")
                    }
                    IconButton(onClick = { actions.onDisconnect() }) {
                        Icon(Icons.AutoMirrored.Filled.Logout, contentDescription = "Disconnect")
                    }
                }
                MainTab.Files -> IconButton(onClick = { actions.onRefreshFiles() }) {
                    Icon(Icons.Default.Refresh, contentDescription = "Refresh files")
                }
                MainTab.Gallery -> IconButton(onClick = { actions.onLoadGallery(true) }) {
                    Icon(Icons.Default.Refresh, contentDescription = "Refresh gallery")
                }
                MainTab.Vault -> IconButton(onClick = { actions.onLoadVault(true) }) {
                    Icon(Icons.Default.Refresh, contentDescription = "Refresh vault")
                }
                MainTab.Search -> Unit
            }
        },
    )
}

@Composable
private fun SetupScreen(
    modifier: Modifier = Modifier,
    initialUrl: String,
    initialDeviceName: String,
    onSave: (url: String, token: String, deviceName: String, allowInsecure: Boolean, testNow: Boolean) -> Unit,
    onScanQr: () -> Unit,
) {
    var url by rememberSaveable { mutableStateOf(initialUrl) }
    // Never place the raw token in saved instance state.
    var token by remember { mutableStateOf("") }
    var deviceName by rememberSaveable { mutableStateOf(initialDeviceName) }
    var allowInsecure by rememberSaveable { mutableStateOf(false) }
    var testNow by rememberSaveable { mutableStateOf(false) }

    val httpUrl = url.trim().startsWith("http://", ignoreCase = true)
    val fieldsReady = url.isNotBlank() && token.isNotBlank()
    val transportReady = !httpUrl || allowInsecure

    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Spacer(Modifier.height(24.dp))
        Icon(
            Icons.Default.CloudUpload,
            contentDescription = null,
            modifier = Modifier.size(72.dp),
            tint = MaterialTheme.colorScheme.primary,
        )
        Text("Connect to your Nimbo server", style = MaterialTheme.typography.titleLarge)
        Text(
            "Scan the pairing QR code shown by Nimbo's settings (Settings → Connect phone), " +
                "or create a device token and enter it manually. Backups run only on unmetered " +
                "networks (Wi-Fi/Ethernet); an optional connection test below uses whatever " +
                "network is currently active. Use a full-scope token to browse the drive, " +
                "gallery, search and read-only vault; a backup-scoped token only sees Phone Backups.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
        Button(
            onClick = onScanQr,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Icon(Icons.Default.QrCodeScanner, contentDescription = null)
            Spacer(Modifier.width(8.dp))
            Text("Scan QR code")
        }
        Text(
            "or pair manually",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
        OutlinedTextField(
            value = url,
            onValueChange = { url = it },
            label = { Text("Server URL") },
            placeholder = { Text("https://nimbo.example.com") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = token,
            onValueChange = { token = it },
            label = { Text("Device token") },
            placeholder = { Text("nmb_…") },
            singleLine = true,
            visualTransformation = PasswordVisualTransformation(),
            supportingText = { Text("Stored encrypted with an Android Keystore-backed key.") },
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = deviceName,
            onValueChange = { deviceName = it },
            label = { Text("Device name") },
            supportingText = { Text("Backups are filed under Phone Backups/<device name>") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )

        if (httpUrl) {
            Text(
                "Insecure HTTP is disabled by default. It exposes your device token and every " +
                    "backed-up file to anyone on the network path. Use HTTPS unless you fully " +
                    "understand this risk.",
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodySmall,
                textAlign = TextAlign.Center,
            )
            CheckRow(
                checked = allowInsecure,
                onCheckedChange = { allowInsecure = it },
                text = "I understand the risk and want to allow insecure HTTP for this server.",
            )
        }

        CheckRow(
            checked = testNow,
            onCheckedChange = { testNow = it },
            text = "Send a connection test now over the current network (may use mobile data).",
        )

        Button(
            onClick = {
                onSave(url.trim(), token.trim(), deviceName.trim(), allowInsecure, true)
            },
            enabled = fieldsReady && transportReady && testNow,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Test & Save")
        }
        OutlinedButton(
            onClick = {
                onSave(url.trim(), token.trim(), deviceName.trim(), allowInsecure, false)
            },
            enabled = fieldsReady && transportReady,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Save without testing")
        }
    }
}

@Composable
private fun CheckRow(
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    text: String,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Checkbox(checked = checked, onCheckedChange = onCheckedChange)
        Text(
            text,
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.weight(1f),
        )
    }
}

@Composable
private fun FolderScreen(
    modifier: Modifier = Modifier,
    state: UiState,
    actions: AppActions,
) {
    Column(modifier = modifier.fillMaxSize()) {
        if (state.authFailed) {
            ErrorBanner(
                "Server rejected the device token. Re-pair with a fresh token; pending files are kept.",
            )
        }
        state.observerError?.let { message ->
            ErrorBanner(message, onDismiss = actions.onDismissObserverError)
        }
        Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Column(Modifier.weight(1f)) {
                    Text("Real-time backup", style = MaterialTheme.typography.titleMedium)
                    Text(
                        "Best-effort instant uploads on unmetered networks. Android 15+ may stop " +
                            "this after a time limit; the 15-minute scan keeps running.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Switch(checked = state.instantMode, onCheckedChange = { actions.onToggleInstant(it) })
            }
            Text(
                "Backed up ${formatBytes(state.totalUploaded)} across ${state.folders.size} folder(s)",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            TextButton(onClick = { actions.onBackupNow() }) {
                Icon(Icons.Default.CloudUpload, contentDescription = null, Modifier.size(18.dp))
                Spacer(Modifier.width(6.dp))
                Text("Backup now")
            }
        }

        if (state.folders.isEmpty()) {
            EmptyHint()
        }

        LazyColumn(
            verticalArrangement = Arrangement.spacedBy(8.dp),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(
                start = 16.dp, end = 16.dp, bottom = 96.dp,
            ),
        ) {
            items(state.folders, key = { it.folder.id }) { ui ->
                FolderCard(ui, actions)
            }
        }
    }
}

@Composable
private fun FolderCard(ui: FolderUi, actions: AppActions) {
    val f = ui.folder
    val counts = ui.counts
    Card(modifier = Modifier.fillMaxWidth()) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(12.dp),
        ) {
            Column(Modifier.weight(1f)) {
                Text(f.displayName, style = MaterialTheme.typography.titleMedium)
                Text(
                    f.remotePath ?: "waiting for first backup…",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (counts != null) {
                    val parts = buildList {
                        if (counts.uploaded > 0) add("${counts.uploaded} backed up")
                        if (counts.pending > 0) add("${counts.pending} pending")
                        if (counts.failed > 0) add("${counts.failed} failed")
                        if (counts.skipped > 0) add("${counts.skipped} skipped")
                    }
                    if (parts.isNotEmpty()) {
                        Text(
                            parts.joinToString(" · "),
                            style = MaterialTheme.typography.bodySmall,
                            color = when {
                                counts.failed > 0 || counts.skipped > 0 -> MaterialTheme.colorScheme.error
                                counts.pending > 0 -> MaterialTheme.colorScheme.secondary
                                else -> MaterialTheme.colorScheme.onSurfaceVariant
                            },
                        )
                    }
                    if (counts.failed > 0 || counts.skipped > 0) {
                        TextButton(onClick = { actions.onRetryFailed(f.id) }) {
                            Text("Retry failed & skipped")
                        }
                    }
                }
                ui.lastError?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.error,
                        maxLines = 2,
                    )
                }
                f.lastScanAt?.let {
                    Text(
                        "Last checked ${formatTime(it)}",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            Switch(checked = f.enabled, onCheckedChange = { actions.onToggleFolder(f.id, it) })
            IconButton(onClick = { actions.onRemoveFolder(f.id) }) {
                Icon(
                    Icons.Default.Delete,
                    contentDescription = "Remove ${f.displayName}",
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun ErrorBanner(text: String, onDismiss: (() -> Unit)? = null) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp),
    ) {
        Icon(
            Icons.Default.Error,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.error,
        )
        Spacer(Modifier.width(8.dp))
        Text(
            text,
            color = MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.weight(1f),
        )
        if (onDismiss != null) {
            TextButton(onClick = onDismiss) { Text("Dismiss") }
        }
    }
}

@Composable
private fun EmptyHint() {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(32.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            "No folders selected yet",
            style = MaterialTheme.typography.titleMedium,
            textAlign = TextAlign.Center,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            "Tap + and pick a folder that Android's file picker allows (for example a camera " +
                "roll or a media folder). Android blocks internal-storage roots, Download and " +
                "other apps' Android/data folders from this picker.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
    }
}

private fun defaultDeviceName(): String =
    android.os.Build.MODEL?.takeIf { it.isNotBlank() } ?: "Phone"

private fun formatBytes(bytes: Long): String = when {
    bytes >= 1 shl 30 -> String.format(Locale.US, "%.1f GB", bytes / 1073741824.0)
    bytes >= 1 shl 20 -> String.format(Locale.US, "%.1f MB", bytes / 1048576.0)
    bytes >= 1 shl 10 -> String.format(Locale.US, "%.1f KB", bytes / 1024.0)
    else -> "$bytes B"
}

private fun formatTime(millis: Long): String =
    android.text.format.DateUtils.getRelativeTimeSpanString(millis).toString()
