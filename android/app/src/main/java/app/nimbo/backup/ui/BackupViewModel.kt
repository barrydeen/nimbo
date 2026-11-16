package app.nimbo.backup.ui

import android.app.Application
import android.content.Context
import android.net.Uri
import androidx.documentfile.provider.DocumentFile
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import app.nimbo.backup.data.Prefs
import app.nimbo.backup.data.ServerConfig
import app.nimbo.backup.data.api.NimboClient
import app.nimbo.backup.data.db.AppDb
import app.nimbo.backup.data.db.FolderCounts
import app.nimbo.backup.data.db.FolderEntity
import app.nimbo.backup.data.isConfigured
import app.nimbo.backup.sync.BackupWorker
import app.nimbo.backup.sync.MediaObserverService
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

data class FolderUi(
    val folder: FolderEntity,
    val counts: FolderCounts?,
)

data class UiState(
    val loading: Boolean = true,
    val configured: Boolean = false,
    val serverUrl: String = "",
    val token: String = "",
    val deviceName: String = "",
    val instantMode: Boolean = false,
    val authFailed: Boolean = false,
    val folders: List<FolderUi> = emptyList(),
    val totalUploaded: Long = 0,
    val snackbar: String? = null,
)

class BackupViewModel(app: Application) : AndroidViewModel(app) {

    private val prefs = Prefs(app)
    private val db = AppDb.get(app)
    private val context: Context = app

    private val snackbar = MutableStateFlow<String?>(null)
    private val loading = MutableStateFlow(true)

    private data class PrefsState(
        val config: ServerConfig?,
        val instantMode: Boolean,
        val authFailed: Boolean,
    )

    private data class DbState(
        val folders: List<FolderEntity>,
        val counts: List<FolderCounts>,
        val totalUploaded: Long,
    )

    val state: StateFlow<UiState> = combine(
        combine(prefs.config, prefs.instantMode, prefs.authFailed) { config, instant, authFailed ->
            PrefsState(config, instant, authFailed)
        },
        combine(
            db.folderDao().observeAll(),
            db.fileDao().observeCounts(),
            db.fileDao().observeTotalUploaded(),
        ) { folders, counts, total -> DbState(folders, counts, total) },
        snackbar,
        loading,
    ) { prefsState, dbState, snack, loadingNow ->
        UiState(
            loading = loadingNow,
            configured = prefsState.config?.isConfigured == true,
            serverUrl = prefsState.config?.serverUrl ?: "",
            token = prefsState.config?.token ?: "",
            deviceName = prefsState.config?.deviceName ?: "",
            instantMode = prefsState.instantMode,
            authFailed = prefsState.authFailed,
            folders = dbState.folders.map { f ->
                FolderUi(f, dbState.counts.firstOrNull { it.folderId == f.id })
            },
            totalUploaded = dbState.totalUploaded,
            snackbar = snack,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), UiState())

    init {
        // Safety net: make sure periodic work exists whenever the app is opened.
        viewModelScope.launch(Dispatchers.IO) {
            if (prefs.config.first()?.isConfigured == true) {
                BackupWorker.schedulePeriodic(context)
            }
            loading.value = false
        }
    }

    fun consumeSnackbar() {
        snackbar.value = null
    }

    fun testAndSave(serverUrl: String, token: String, deviceName: String) {
        viewModelScope.launch(Dispatchers.IO) {
            try {
                NimboClient(serverUrl, token).testConnection()
                prefs.save(ServerConfig(serverUrl, token, deviceName))
                if (state.value.instantMode) MediaObserverService.maybeStart(context)
                BackupWorker.requestSync(context)
            } catch (e: Exception) {
                snackbar.value = "Connection failed: ${e.message}"
            }
        }
    }

    fun addFolder(treeUri: Uri) {
        viewModelScope.launch(Dispatchers.IO) {
            try {
                context.contentResolver.takePersistableUriPermission(
                    treeUri,
                    android.content.Intent.FLAG_GRANT_READ_URI_PERMISSION,
                )
            } catch (_: SecurityException) {
                // some providers only grant write; persistable flag still survives
                try {
                    context.contentResolver.takePersistableUriPermission(
                        treeUri,
                        android.content.Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
                    )
                } catch (_: SecurityException) {
                    snackbar.value = "could not keep access to that folder"
                    return@launch
                }
            }
            val name = DocumentFile.fromTreeUri(context, treeUri)?.name
                ?: treeUri.lastPathSegment?.substringAfter(':')
                ?: "folder"
            db.folderDao().insert(
                FolderEntity(
                    treeUri = treeUri.toString(),
                    displayName = name,
                    createdAt = System.currentTimeMillis(),
                ),
            )
            BackupWorker.requestSync(context)
        }
    }

    fun toggleFolder(id: Long, enabled: Boolean) {
        viewModelScope.launch(Dispatchers.IO) { db.folderDao().setEnabled(id, enabled) }
    }

    fun removeFolder(id: Long) {
        viewModelScope.launch(Dispatchers.IO) {
            db.folderDao().delete(id)
            db.fileDao().deleteAllForFolder(id)
        }
    }

    fun setInstantMode(enabled: Boolean) {
        viewModelScope.launch(Dispatchers.IO) {
            prefs.setInstantMode(enabled)
            if (enabled) MediaObserverService.maybeStart(context)
            else MediaObserverService.stop(context)
        }
    }

    fun backupNow() {
        BackupWorker.requestSync(context)
        snackbar.value = "Backup queued — runs over Wi-Fi"
    }

    fun disconnect() {
        viewModelScope.launch(Dispatchers.IO) {
            MediaObserverService.stop(context)
            androidx.work.WorkManager.getInstance(context).cancelUniqueWork(BackupWorker.PERIODIC)
            // Keep folders/history so re-pairing resumes seamlessly.
            Prefs(context).save(ServerConfig("", "", state.value.deviceName))
            // save() with blanks is rejected by isConfigured but still writes; that's fine —
            // config flow emits null when url/token are blank.
        }
    }
}
