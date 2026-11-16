package app.nimbo.backup.ui

import android.app.Application
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import androidx.documentfile.provider.DocumentFile
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import app.nimbo.backup.data.Prefs
import app.nimbo.backup.data.InsecureTransportException
import app.nimbo.backup.data.ServerConfig
import app.nimbo.backup.data.ServerOrigin
import app.nimbo.backup.data.api.DocSummary
import app.nimbo.backup.data.api.NodeSummary
import app.nimbo.backup.data.api.NimboApiException
import app.nimbo.backup.data.api.NimboAuthException
import app.nimbo.backup.data.api.NimboClient
import app.nimbo.backup.data.api.InstanceInfo
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
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.io.IOException

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
    val instanceName: String = "",
    /** null when the scope has not been probed yet. */
    val backupScope: Boolean? = null,
)

class BackupViewModel(app: Application) : AndroidViewModel(app) {

    private val prefs = Prefs(app)
    private val db = AppDb.get(app)
    private val context: Context get() = getApplication<Application>().applicationContext

    private val snackbar = MutableStateFlow<String?>(null)
    private val loading = MutableStateFlow(true)
    private val instance = MutableStateFlow<InstanceInfo?>(null)
    private val browse = MutableStateFlow(BrowseState())

    /** Remote browsing state (files, gallery, search and the read-only vault). */
    val browseState: StateFlow<BrowseState> = browse.asStateFlow()

    @Volatile
    private var cachedClient: NimboClient? = null
    @Volatile
    private var cachedConfig: ServerConfig? = null

    private var filesGeneration = 0
    private var galleryGeneration = 0
    private var vaultGeneration = 0
    private var docGeneration = 0
    private var searchJob: Job? = null

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
        instance,
    ) { prefsState, dbState, snack, loadingNow, instanceInfo ->
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
            instanceName = instanceInfo?.instance_name.orEmpty(),
            backupScope = instanceInfo?.backup_scope,
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
            if (prefs.config.first()?.isConfigured == true && !prefs.authFailed.first()) {
                probeInstance()
            }
        }
    }

    private suspend fun probeInstance() {
        try {
            instance.value = client().instance()
        } catch (e: Exception) {
            if (e is NimboAuthException) {
                prefs.markAuthFailed()
            } else {
                SafeLog.w(TAG, "instance probe failed", e)
            }
        }
    }

    private suspend fun client(): NimboClient {
        val config = cachedConfig ?: prefs.config.first()
            ?: throw IOException("Pair this device first")
        val cached = cachedClient
        if (cached != null && cachedConfig == config) return cached
        return synchronized(this) {
            val current = cachedClient
            if (current != null && cachedConfig == config) {
                current
            } else {
                NimboClient(config.serverUrl, config.token, config.allowInsecureHttp).also {
                    cachedClient = it
                    cachedConfig = config
                }
            }
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
                val tested = if (testNow) {
                    NimboClient(
                        normalized.rootUrl,
                        config.token,
                        allowInsecureHttp,
                    ).testConnection()
                } else {
                    null
                }
                prefs.save(config)
                cachedClient = null
                cachedConfig = null
                instance.value = tested
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
                cachedClient = null
                cachedConfig = null
                instance.value = null
                browse.value = BrowseState()
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

    // ------------------------------------------------------------ remote files

    fun openFolder(folderId: String) {
        val generation = ++filesGeneration
        val current = browse.value.files
        browse.update {
            it.copy(
                files = it.files.copy(
                    folderId = folderId,
                    folderName = if (folderId.isBlank()) "My Drive" else it.files.folderName,
                    loading = true,
                    error = null,
                    items = if (folderId != current.folderId) emptyList() else it.files.items,
                ),
            )
        }
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val listing = client().listFolder(folderId)
                if (generation != filesGeneration) return@launch
                val id = normalizeFolderId(listing.folder.id)
                browse.update {
                    it.copy(
                        files = FilesState(
                            folderId = id,
                            folderName = listing.folder.name,
                            breadcrumbs = listing.breadcrumbs.map { crumb ->
                                CrumbUi(normalizeFolderId(crumb.id), crumb.name)
                            },
                            items = listing.items,
                            loading = false,
                        ),
                    )
                }
            } catch (e: Exception) {
                if (generation != filesGeneration) return@launch
                val message = describeError(e)
                browse.update { it.copy(files = it.files.copy(loading = false, error = message)) }
            }
        }
    }

    fun refreshFiles() {
        openFolder(browse.value.files.folderId)
    }

    fun goToParentFolder() {
        val crumbs = browse.value.files.breadcrumbs
        val parent = if (crumbs.size >= 2) crumbs[crumbs.size - 2].id else ""
        openFolder(parent)
    }

    fun openItem(item: NodeSummary) {
        when {
            item.kind == "folder" -> openFolder(item.id)
            item.mime?.startsWith("image/") == true -> browse.update { it.copy(imageViewer = item) }
            item.mime?.startsWith("video/") == true -> browse.update { it.copy(imageViewer = item) }
            else -> snackbar.value = "Tap the download button to save this file"
        }
    }

    fun openImageViewer(item: NodeSummary) {
        browse.update { it.copy(imageViewer = item) }
    }

    fun closeImageViewer() {
        browse.update { it.copy(imageViewer = null) }
    }

    // --------------------------------------------------------------- gallery

    fun setMediaFilter(filter: MediaFilter) {
        if (browse.value.gallery.filter == filter) return
        galleryGeneration++
        browse.update {
            it.copy(
                gallery = it.gallery.copy(
                    filter = filter,
                    items = emptyList(),
                    nextOffset = null,
                    error = null,
                ),
            )
        }
        loadGallery(reset = true)
    }

    fun loadGallery(reset: Boolean = false) {
        val gallery = browse.value.gallery
        if (gallery.loading) return
        if (!reset && gallery.nextOffset == null && gallery.items.isNotEmpty()) return
        val generation = if (reset) ++galleryGeneration else galleryGeneration
        val filter = gallery.filter
        val offset = if (reset) 0 else gallery.items.size
        browse.update {
            it.copy(
                gallery = it.gallery.copy(
                    loading = true,
                    error = null,
                    items = if (reset) emptyList() else it.gallery.items,
                    nextOffset = if (reset) null else it.gallery.nextOffset,
                ),
            )
        }
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val page = client().media(filter.apiValue, offset, GALLERY_PAGE)
                if (generation != galleryGeneration) return@launch
                browse.update {
                    it.copy(
                        gallery = it.gallery.copy(
                            items = if (reset) page.items else it.gallery.items + page.items,
                            nextOffset = page.next_offset,
                            loading = false,
                        ),
                    )
                }
            } catch (e: Exception) {
                if (generation != galleryGeneration) return@launch
                val message = describeError(e)
                browse.update { it.copy(gallery = it.gallery.copy(loading = false, error = message)) }
            }
        }
    }

    // ---------------------------------------------------------------- search

    fun search(query: String) {
        browse.update {
            it.copy(
                search = it.search.copy(
                    query = query,
                    loading = query.isNotBlank(),
                    error = null,
                    searched = false,
                    results = if (query.isBlank()) emptyList() else it.search.results,
                ),
            )
        }
        searchJob?.cancel()
        val trimmed = query.trim()
        if (trimmed.isEmpty()) return
        searchJob = viewModelScope.launch(Dispatchers.IO) {
            delay(350)
            try {
                val results = client().searchFiles(trimmed)
                if (browse.value.search.query != query) return@launch
                browse.update {
                    it.copy(
                        search = it.search.copy(
                            results = results,
                            loading = false,
                            searched = true,
                        ),
                    )
                }
            } catch (e: Exception) {
                if (browse.value.search.query != query) return@launch
                val message = describeError(e)
                browse.update {
                    it.copy(search = it.search.copy(loading = false, error = message, searched = true))
                }
            }
        }
    }

    // ----------------------------------------------------------- read-only vault

    fun loadVault(force: Boolean = false) {
        if (instance.value?.backup_scope == true) {
            browse.update {
                it.copy(vault = it.vault.copy(loading = false, loaded = true, error = null))
            }
            return
        }
        val vault = browse.value.vault
        if (vault.loading || (vault.loaded && !force)) return
        val generation = ++vaultGeneration
        browse.update { it.copy(vault = it.vault.copy(loading = true, error = null)) }
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val documents = client().docsTree()
                if (generation != vaultGeneration) return@launch
                browse.update {
                    it.copy(
                        vault = it.vault.copy(
                            documents = documents,
                            loading = false,
                            loaded = true,
                        ),
                    )
                }
            } catch (e: Exception) {
                if (generation != vaultGeneration) return@launch
                val message = describeError(e)
                browse.update { it.copy(vault = it.vault.copy(loading = false, error = message)) }
            }
        }
    }

    fun openDoc(document: DocSummary) {
        val generation = ++docGeneration
        browse.update {
            it.copy(
                docViewer = DocViewerState(
                    path = document.path,
                    title = document.title,
                    loading = true,
                ),
            )
        }
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val content = client().docContent(document.path)
                if (generation != docGeneration) return@launch
                browse.update {
                    it.copy(
                        docViewer = DocViewerState(
                            path = content.path,
                            title = content.title.ifBlank { document.title },
                            content = content.content,
                            loading = false,
                        ),
                    )
                }
            } catch (e: Exception) {
                if (generation != docGeneration) return@launch
                val message = describeError(e)
                browse.update {
                    it.copy(docViewer = it.docViewer?.copy(loading = false, error = message))
                }
            }
        }
    }

    fun closeDoc() {
        docGeneration++
        browse.update { it.copy(docViewer = null) }
    }

    // -------------------------------------------------------------- downloads

    fun downloadTarget(item: NodeSummary): DownloadTarget =
        DownloadTarget(id = item.id, name = item.name, mime = item.mime)

    /** Saves through MediaStore on Android 10+; older versions use the SAF picker. */
    fun download(item: NodeSummary) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return
        val id = item.id
        if (id in browse.value.downloading) return
        browse.update { it.copy(downloading = it.downloading + id) }
        viewModelScope.launch(Dispatchers.IO) {
            var uri: Uri? = null
            try {
                val values = ContentValues().apply {
                    put(MediaStore.Downloads.DISPLAY_NAME, safeDownloadName(item.name))
                    put(MediaStore.Downloads.MIME_TYPE, item.mime ?: "application/octet-stream")
                    put(
                        MediaStore.Downloads.RELATIVE_PATH,
                        "${Environment.DIRECTORY_DOWNLOADS}/Nimbo",
                    )
                    put(MediaStore.Downloads.IS_PENDING, 1)
                }
                val resolver = context.contentResolver
                uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
                    ?: throw IOException("Could not create a download entry")
                val destination = uri
                val stream = resolver.openOutputStream(destination)
                    ?: throw IOException("Could not open the download destination")
                stream.use { output -> client().downloadTo(id, output) }
                resolver.update(
                    destination,
                    ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) },
                    null,
                    null,
                )
                snackbar.value = "Saved ${item.name} to Downloads/Nimbo"
            } catch (e: Exception) {
                uri?.let { pending ->
                    runCatching { context.contentResolver.delete(pending, null, null) }
                }
                snackbar.value = "Download failed: ${describeError(e)}"
            } finally {
                browse.update { it.copy(downloading = it.downloading - id) }
            }
        }
    }

    /** Completes a download into a user-chosen SAF document URI. */
    fun saveDownloadTo(target: DownloadTarget, uri: Uri) {
        if (target.id in browse.value.downloading) return
        browse.update { it.copy(downloading = it.downloading + target.id) }
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val resolver = context.contentResolver
                val stream = resolver.openOutputStream(uri)
                    ?: throw IOException("Could not open the selected file")
                stream.use { output -> client().downloadTo(target.id, output) }
                snackbar.value = "Saved ${target.name}"
            } catch (e: Exception) {
                runCatching { context.contentResolver.delete(uri, null, null) }
                snackbar.value = "Download failed: ${describeError(e)}"
            } finally {
                browse.update { it.copy(downloading = it.downloading - target.id) }
            }
        }
    }

    suspend fun loadThumbnail(item: NodeSummary): ByteArray? = try {
        if (item.has_thumb) client().thumbnail(item.id) else null
    } catch (e: Exception) {
        if (e is NimboAuthException) prefs.markAuthFailed()
        null
    }

    suspend fun loadFullImage(item: NodeSummary): ByteArray? = try {
        if (item.size > MAX_INLINE_IMAGE_BYTES) null else client().contentBytes(item.id)
    } catch (e: Exception) {
        if (e is NimboAuthException) prefs.markAuthFailed()
        null
    }

    private fun safeDownloadName(name: String): String {
        val cleaned = name.trim().map { ch ->
            if (ch == '/' || ch == '\\' || ch.isISOControl()) '_' else ch
        }.joinToString("")
        return cleaned.ifBlank { "nimbo-download" }
    }

    private fun normalizeFolderId(id: String): String = if (id == "root") "" else id

    private suspend fun describeError(e: Exception): String {
        if (e is NimboAuthException) {
            prefs.markAuthFailed()
            return "The device token was rejected. Re-pair to continue."
        }
        if (e is NimboApiException) {
            return when (e.code) {
                403 -> "This device token is backup-only. Pair a full-scope token to browse the drive and vault."
                404 -> "That item no longer exists."
                413 -> "That item is too large to preview."
                429 -> "The server is busy. Try again shortly."
                else -> e.message ?: "Server error ${e.code}"
            }
        }
        return e.message ?: "Network error"
    }

    fun disconnect() {
        viewModelScope.launch(Dispatchers.IO) {
            MediaObserverService.stop(context)
            BackupWorker.cancelAll(context)
            BackupEngine.serialized { prefs.clearPairing() }
            cachedClient = null
            cachedConfig = null
            instance.value = null
            browse.value = BrowseState()
            snackbar.value = "Disconnected. Folder selections are kept for re-pairing."
        }
    }

    companion object {
        private const val TAG = "NimboBackup"
        private const val GALLERY_PAGE = 120
        private const val MAX_INLINE_IMAGE_BYTES = 40L * 1024 * 1024
    }
}
