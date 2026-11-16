package app.nimbo.backup.ui

import android.app.Application
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.documentfile.provider.DocumentFile
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import app.nimbo.backup.data.Prefs
import app.nimbo.backup.data.InsecureTransportException
import app.nimbo.backup.data.ServerConfig
import app.nimbo.backup.data.ServerOrigin
import app.nimbo.backup.data.api.NimboClient
import app.nimbo.backup.data.db.AppDb
import app.nimbo.backup.data.db.FileFailure
import app.nimbo.backup.data.db.FolderCounts
import app.nimbo.backup.data.db.FolderEntity
import app.nimbo.backup.data.isConfigured
import app.nimbo.backup.sync.BackupEngine
import app.nimbo.backup.sync.BackupWorker
import app.nimbo.backup.sync.MediaObserverService
import app.nimbo.backup.util.SafeLog
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

data class FolderUi(
    val folder: FolderEntity,
    val counts: FolderCounts?,
    val lastError: String?,
)

data class UiState(
    val loading: Boolean = true,
    val configured: Boolean = false,
    val serverUrl: String = "",
    val deviceName: String = "",
    val allowInsecureHttp: Boolean = false,
    val instantMode: Boolean = false,
    val authFailed: Boolean = false,
    val observerError: String? = null,
    val folders: List<FolderUi> = emptyList(),
    val totalUploaded: Long = 0,
    val snackbar: String? = null,
)

class BackupViewModel(app: Application) : AndroidViewModel(app) {

    private val prefs = Prefs(app)
    private val db = AppDb.get(app)
    private val context: Context get() = getApplication<Application>().applicationContext

    private val snackbar = MutableStateFlow<String?>(null)
    private val loading = MutableStateFlow(true)

    private data class PrefsState(
        val config: ServerConfig?,
        val instantMode: Boolean,
        val authFailed: Boolean,
        val observerError: String?,
    )

    private data class DbState(
        val folders: List<FolderEntity>,
        val counts: List<FolderCounts>,
        val failures: List<FileFailure>,
        val totalUploaded: Long,
    )

    val state: StateFlow<UiState> = combine(
        combine(
            prefs.config,
            prefs.instantMode,
            prefs.authFailed,
            prefs.observerError,
        ) { config, instant, authFailed, observerError ->
            PrefsState(config, instant, authFailed, observerError?.takeIf { it.isNotBlank() })
        },
        combine(
            db.folderDao().observeAll(),
            db.fileDao().observeCounts(),
            db.fileDao().observeFailures(),
            db.fileDao().observeTotalUploaded(),
        ) { folders, counts, failures, total -> DbState(folders, counts, failures, total) },
        snackbar,
        loading,
    ) { prefsState, dbState, snack, loadingNow ->
        val errors = dbState.failures.groupBy { it.folderId }.mapValues { it.value.first().lastError }
        UiState(
            loading = loadingNow,
            configured = prefsState.config?.isConfigured == true,
            serverUrl = prefsState.config?.serverUrl ?: "",
            deviceName = prefsState.config?.deviceName ?: "",
            allowInsecureHttp = prefsState.config?.allowInsecureHttp ?: false,
            instantMode = prefsState.instantMode,
            authFailed = prefsState.authFailed,
            observerError = prefsState.observerError,
            folders = dbState.folders.map { folder ->
                FolderUi(
                    folder = folder,
                    counts = dbState.counts.firstOrNull { it.folderId == folder.id },
                    lastError = errors[folder.id],
                )
            },
            totalUploaded = dbState.totalUploaded,
            snackbar = snack,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), UiState())

    init {
        viewModelScope.launch(Dispatchers.IO) {
            prefs.migrateLegacy()
            if (prefs.config.first()?.isConfigured == true) {
                if (!prefs.authFailed.first()) {
                    BackupWorker.schedulePeriodic(context)
                    if (prefs.instantMode.first()) MediaObserverService.maybeStart(context)
                }
            }
            loading.value = false
        }
    }

    fun consumeSnackbar() {
        snackbar.value = null
    }

    fun testAndSave(
        serverUrl: String,
        token: String,
        deviceName: String,
        allowInsecureHttp: Boolean,
        testNow: Boolean,
    ) {
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val normalized = ServerOrigin.parse(serverUrl, allowInsecureHttp)
                val config = ServerConfig(
                    serverUrl = normalized.rootUrl,
                    token = token.trim(),
                    deviceName = deviceName.trim(),
                    allowInsecureHttp = allowInsecureHttp,
                )
                if (testNow) NimboClient(normalized.rootUrl, config.token, allowInsecureHttp).testConnection()
                prefs.save(config)
                if (state.value.instantMode) MediaObserverService.maybeStart(context)
                if (testNow) BackupWorker.requestSync(context)
                snackbar.value = if (testNow) "Connected and saved" else "Saved without a connection test"
            } catch (e: Exception) {
                SafeLog.w(TAG, "save/test failed", e)
                snackbar.value = "Connection failed: ${e.message ?: "unexpected error"}"
            }
        }
    }

    /**
     * Applies a pairing QR without making a network request. HTTPS is required;
     * an HTTP payload cannot bypass the explicit insecure-transport warning.
     */
    fun pairFromQr(serverUrl: String, token: String) {
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val normalized = ServerOrigin.parse(serverUrl, allowInsecure = false)
                val deviceName = state.value.deviceName.ifBlank { android.os.Build.MODEL ?: "Phone" }
                prefs.save(
                    ServerConfig(
                        serverUrl = normalized.rootUrl,
                        token = token.trim(),
                        deviceName = deviceName,
                        allowInsecureHttp = false,
                    ),
                )
                if (state.value.instantMode) MediaObserverService.maybeStart(context)
                snackbar.value = "Paired. Backups run on unmetered networks."
            } catch (_: InsecureTransportException) {
                snackbar.value =
                    "This QR code uses insecure HTTP. Enter the server manually and explicitly enable HTTP."
            } catch (e: Exception) {
                SafeLog.w(TAG, "QR pairing failed", e)
                snackbar.value = "Could not save that pairing code"
            }
        }
    }

    fun reportBadQr() {
        snackbar.value = "That QR code is not a Nimbo pairing code"
    }

    fun addFolder(treeUri: Uri) {
        viewModelScope.launch(Dispatchers.IO) {
            try {
                context.contentResolver.takePersistableUriPermission(
                    treeUri,
                    Intent.FLAG_GRANT_READ_URI_PERMISSION,
                )
            } catch (_: SecurityException) {
                try {
                    context.contentResolver.takePersistableUriPermission(
                        treeUri,
                        Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
                    )
                } catch (_: SecurityException) {
                    snackbar.value = "Could not keep access to that folder"
                    return@launch
                }
            }
            val name = DocumentFile.fromTreeUri(context, treeUri)?.name
                ?: treeUri.lastPathSegment?.substringAfter(':')
                ?: "folder"
            val id = db.folderDao().insert(
                FolderEntity(
                    treeUri = treeUri.toString(),
                    displayName = name.trim().ifBlank { "folder" },
                    createdAt = System.currentTimeMillis(),
                ),
            )
            if (id == -1L) {
                snackbar.value = "That folder is already selected"
                return@launch
            }
            BackupWorker.requestSync(context)
        }
    }

    fun toggleFolder(id: Long, enabled: Boolean) {
        viewModelScope.launch(Dispatchers.IO) { db.folderDao().setEnabled(id, enabled) }
    }

    fun removeFolder(id: Long) {
        viewModelScope.launch(Dispatchers.IO) {
            val folder = try {
                BackupEngine.serialized {
                    db.folderDao().byId(id)?.also { db.removeFolder(id) }
                }
            } catch (e: Exception) {
                SafeLog.w(TAG, "folder removal failed", e)
                snackbar.value = "Could not remove that folder"
                return@launch
            }
            if (folder != null) releasePersistedGrant(folder.treeUri)
        }
    }

    private fun releasePersistedGrant(treeUri: String) {
        val uri = Uri.parse(treeUri)
        for (flag in intArrayOf(
            Intent.FLAG_GRANT_READ_URI_PERMISSION,
            Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
        )) {
            try {
                context.contentResolver.releasePersistableUriPermission(uri, flag)
            } catch (_: SecurityException) {
                // Grant was not held (or provider already revoked it).
            }
        }
    }

    fun retryFailed(folderId: Long) {
        viewModelScope.launch(Dispatchers.IO) {
            BackupEngine.serialized { db.fileDao().resetFailures(folderId) }
            BackupWorker.requestSync(context)
            snackbar.value = "Failed files queued for another attempt"
        }
    }

    fun setInstantMode(enabled: Boolean) {
        viewModelScope.launch(Dispatchers.IO) {
            prefs.setInstantMode(enabled)
            if (enabled) MediaObserverService.maybeStart(context) else MediaObserverService.stop(context)
        }
    }

    fun backupNow() {
        if (state.value.authFailed) {
            snackbar.value = "Re-pair the device token before backing up"
            return
        }
        BackupWorker.requestSync(context)
        snackbar.value = "Backup queued — runs on unmetered networks"
    }

    fun dismissObserverError() {
        viewModelScope.launch(Dispatchers.IO) { prefs.setObserverError(null) }
    }

    fun disconnect() {
        viewModelScope.launch(Dispatchers.IO) {
            MediaObserverService.stop(context)
            BackupWorker.cancelAll(context)
            BackupEngine.serialized { prefs.clearPairing() }
            snackbar.value = "Disconnected. Folder selections are kept for re-pairing."
        }
    }

    companion object {
        private const val TAG = "NimboBackup"
    }
}
