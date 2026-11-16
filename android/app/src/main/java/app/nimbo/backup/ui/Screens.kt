package app.nimbo.backup.ui

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
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.CloudUpload
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Error
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
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
fun BackupApp(
    state: UiState,
    actions: BackupActions,
) {
    val snackbarHostState = remember { SnackbarHostState() }
    LaunchedEffect(state.snackbar) {
        state.snackbar?.let {
            snackbarHostState.showSnackbar(it)
            actions.onSnackbarConsumed()
        }
    }

    Scaffold(
        topBar = {
            if (state.configured) {
                FolderTopBar(state, actions)
            }
        },
        floatingActionButton = {
            if (state.configured) {
                FloatingActionButton(onClick = { actions.onAddFolder() }) {
                    Icon(Icons.Default.Add, contentDescription = "Add folder")
                }
            }
        },
        snackbarHost = {
            SnackbarHost(snackbarHostState) { data ->
                Snackbar(snackbarData = data)
            }
        },
    ) { padding ->
        when {
            state.loading -> Box(
                modifier = Modifier.fillMaxSize().padding(padding),
                contentAlignment = Alignment.Center,
            ) { CircularProgressIndicator() }

            !state.configured -> SetupScreen(
                Modifier.padding(padding),
                initialUrl = state.serverUrl,
                initialToken = state.token,
                initialDeviceName = state.deviceName.ifBlank { defaultDeviceName() },
                onSave = { url, token, device -> actions.onTestAndSave(url, token, device) },
            )

            else -> FolderScreen(Modifier.padding(padding), state, actions)
        }
    }
}

class BackupActions(
    val onTestAndSave: (url: String, token: String, deviceName: String) -> Unit,
    val onAddFolder: () -> Unit,
    val onToggleFolder: (id: Long, enabled: Boolean) -> Unit,
    val onRemoveFolder: (id: Long) -> Unit,
    val onToggleInstant: (enabled: Boolean) -> Unit,
    val onBackupNow: () -> Unit,
    val onDisconnect: () -> Unit,
    val onSnackbarConsumed: () -> Unit,
)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun FolderTopBar(state: UiState, actions: BackupActions) {
    TopAppBar(
        title = {
            Column {
                Text("Nimbo Backup")
                Text(
                    state.serverUrl.substringAfter("://"),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        },
        actions = {
            IconButton(onClick = { actions.onBackupNow() }) {
                Icon(Icons.Default.Refresh, contentDescription = "Backup now")
            }
            IconButton(onClick = { actions.onDisconnect() }) {
                Icon(Icons.AutoMirrored.Filled.Logout, contentDescription = "Disconnect")
            }
        },
    )
}

@Composable
private fun SetupScreen(
    modifier: Modifier = Modifier,
    initialUrl: String,
    initialToken: String,
    initialDeviceName: String,
    onSave: (url: String, token: String, deviceName: String) -> Unit,
) {
    var url by rememberSaveable { mutableStateOf(initialUrl) }
    var token by rememberSaveable { mutableStateOf(initialToken) }
    var deviceName by rememberSaveable { mutableStateOf(initialDeviceName) }

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
            "Create a device token in Nimbo under Settings → Device tokens, " +
                "then paste it here. Backups run over Wi-Fi only.",
            style = MaterialTheme.typography.bodyMedium,
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
        Button(
            onClick = { onSave(url.trim(), token.trim(), deviceName.trim()) },
            enabled = url.isNotBlank() && token.isNotBlank(),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Test & Save")
        }
    }
}

@Composable
private fun FolderScreen(
    modifier: Modifier = Modifier,
    state: UiState,
    actions: BackupActions,
) {
    Column(modifier = modifier.fillMaxSize()) {
        if (state.authFailed) {
            ErrorBanner("Server rejected the device token. Paste a fresh token below.")
        }
        Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Column(Modifier.weight(1f)) {
                    Text("Real-time backup", style = MaterialTheme.typography.titleMedium)
                    Text(
                        "Instantly upload photos, videos & downloads (Wi-Fi)",
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
private fun FolderCard(ui: FolderUi, actions: BackupActions) {
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
                    }
                    if (parts.isNotEmpty()) {
                        Text(
                            parts.joinToString(" · "),
                            style = MaterialTheme.typography.bodySmall,
                            color = when {
                                counts.failed > 0 -> MaterialTheme.colorScheme.error
                                counts.pending > 0 -> MaterialTheme.colorScheme.secondary
                                else -> MaterialTheme.colorScheme.onSurfaceVariant
                            },
                        )
                    }
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
private fun ErrorBanner(text: String) {
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
        Text(text, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium)
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
            "Tap + and pick a folder to back up — camera rolls, WhatsApp media, " +
                "Downloads, anything. New files upload automatically over Wi-Fi.",
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
