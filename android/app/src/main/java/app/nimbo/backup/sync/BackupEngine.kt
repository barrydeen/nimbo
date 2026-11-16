package app.nimbo.backup.sync

import android.content.Context
import android.net.Uri
import androidx.documentfile.provider.DocumentFile
import app.nimbo.backup.data.Prefs
import app.nimbo.backup.data.RemoteNameRules
import app.nimbo.backup.data.ServerOrigin
import app.nimbo.backup.data.api.NimboApiException
import app.nimbo.backup.data.api.NimboAuthException
import app.nimbo.backup.data.api.NimboClient
import app.nimbo.backup.data.api.NodeSummary
import app.nimbo.backup.data.db.AppDb
import app.nimbo.backup.data.db.FileEntity
import app.nimbo.backup.data.db.FileStatus
import app.nimbo.backup.data.db.FolderEntity
import app.nimbo.backup.data.isConfigured
import app.nimbo.backup.util.SafeLog
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.firstOrNull
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.security.MessageDigest
import java.util.concurrent.atomic.AtomicBoolean

/**
 * One-way phone → Nimbo backup engine.
 *
 * Per enabled folder:
 *  1. Walk the SAF tree, reconciling every document against the local Room state.
 *  2. Mirror the folder's directory structure on the server under
 *     `Phone Backups/<device name>/<local folder name>/...`.
 *  3. Upload new content or atomically replace the tracked remote node when the
 *     bytes changed. Adoption after reinstall/response loss requires the server
 *     SHA-256 to equal the locally computed SHA-256.
 *
 * Every execution (periodic worker, one-shot worker, disconnect/folder changes)
 * shares one process-wide mutex. Files deleted on the phone keep their server
 * copy; content changes replace the existing node instead of deleting it first.
 */
object BackupEngine {

    private const val TAG = "NimboBackup"
    private const val ROOT_PREFIX = "Phone Backups"
    private const val CACHE_MARGIN_BYTES = 8L * 1024 * 1024

    private val mutex = Mutex()
    private val rerunRequested = AtomicBoolean(false)

    /** Marks that a trigger arrived while a run was active; the run coalesces it. */
    fun requestRerun() {
        rerunRequested.set(true)
    }

    /** Runs arbitrary bookkeeping under the same lock as backup execution. */
    suspend fun <T> serialized(block: suspend () -> T): T = mutex.withLock { block() }

    suspend fun run(context: Context): Outcome = mutex.withLock {
        var result: Outcome = Outcome.NotConfigured
        var passes = 0
        do {
            rerunRequested.set(false)
            result = runOnce(context)
            passes += 1
        } while (rerunRequested.get() && result is Outcome.Ok && passes < 3)
        result
    }

    sealed interface Outcome {
        data object NotConfigured : Outcome
        data object AuthRejected : Outcome
        data class ConfigurationError(val message: String) : Outcome
        data class Ok(
            var transientFailures: Boolean = false,
            val errors: MutableList<String> = mutableListOf(),
        ) : Outcome
    }

    private suspend fun runOnce(context: Context): Outcome {
        val prefs = Prefs(context)
        prefs.migrateLegacy()
        cleanStaleParts(context)

        val config = prefs.config.firstOrNull() ?: return Outcome.NotConfigured
        if (!config.isConfigured) return Outcome.NotConfigured
        if (prefs.authFailed.first()) return Outcome.AuthRejected

        val server = try {
            ServerOrigin.parse(config.serverUrl, config.allowInsecureHttp)
        } catch (e: IllegalArgumentException) {
            return Outcome.ConfigurationError(
                e.message ?: "The saved server URL is no longer usable. Re-pair in the app.",
            )
        }

        val db = AppDb.get(context)
        val client = NimboClient(server.rootUrl, config.token, config.allowInsecureHttp)

        return try {
            val instance = client.instance()
            val accountId = try {
                client.accountId()
            } catch (e: NimboApiException) {
                // Account identity is only a cache-invalidation aid. An older or
                // restricted server must not block backups entirely.
                if (e.code in 400..499) null else throw e
            }
            val maxUpload = instance.max_upload_bytes?.takeIf { it > 0 }
            val outcome = Outcome.Ok()

            for (folder in db.folderDao().enabled()) {
                currentCoroutineContext().ensureActive()
                try {
                    SafeLog.d(TAG, "syncing one enabled folder")
                    syncFolder(
                        context = context,
                        client = client,
                        db = db,
                        folder = folder,
                        deviceName = config.deviceName,
                        origin = server.origin,
                        accountId = accountId,
                        maxUploadBytes = maxUpload,
                        outcome = outcome,
                    )
                } catch (e: CancellationException) {
                    throw e
                } catch (e: NimboAuthException) {
                    prefs.markAuthFailed()
                    MediaObserverService.stop(context)
                    SafeLog.w(TAG, "server rejected device token")
                    return Outcome.AuthRejected
                } catch (e: IOException) {
                    outcome.transientFailures = true
                    outcome.errors += "backup network/server failure"
                    SafeLog.w(TAG, "folder backup i/o failure", e)
                    break
                } catch (e: Exception) {
                    outcome.transientFailures = true
                    outcome.errors += "folder backup failure"
                    SafeLog.w(TAG, "folder backup failure", e)
                }
            }
            outcome
        } catch (e: CancellationException) {
            throw e
        } catch (e: NimboAuthException) {
            prefs.markAuthFailed()
            MediaObserverService.stop(context)
            SafeLog.w(TAG, "server rejected device token")
            Outcome.AuthRejected
        } catch (e: IOException) {
            SafeLog.w(TAG, "backup startup network failure", e)
            Outcome.Ok(transientFailures = true, errors = mutableListOf("backup network failure"))
        } catch (e: Exception) {
            SafeLog.w(TAG, "backup startup failure", e)
            Outcome.Ok(transientFailures = true, errors = mutableListOf("backup startup failure"))
        }
    }

    private suspend fun syncFolder(
        context: Context,
        client: NimboClient,
        db: AppDb,
        folder: FolderEntity,
        deviceName: String,
        origin: String,
        accountId: String?,
        maxUploadBytes: Long?,
        outcome: Outcome.Ok,
    ) {
        val tree = DocumentFile.fromTreeUri(context, Uri.parse(folder.treeUri))
        if (tree == null || !tree.exists()) {
            db.folderDao().setEnabled(folder.id, false)
            throw IOException("folder permission lost — re-add it in the app")
        }

        val cache = RemoteCache(client)
        val identity = RemoteIdentity(origin, accountId)
        val cached = reconcileRemoteIdentity(db, folder, identity)
        val remoteRootId = try {
            ensureRemoteFolder(
                client = client,
                db = db,
                folderId = folder.id,
                cachedId = cached.first,
                cachedPath = cached.second,
                deviceName = deviceName,
                displayName = tree.name ?: folder.displayName,
                cache = cache,
                identity = identity,
            )
        } catch (e: PermanentItemException) {
            val message = e.message ?: "folder name cannot be mirrored"
            db.fileDao().forFolder(folder.id)
                .filter { it.status == FileStatus.PENDING || it.status == FileStatus.FAILED }
                .forEach { row ->
                    db.fileDao().markFailed(row.id, FileStatus.SKIPPED, message)
                }
            db.folderDao().setLastScan(folder.id, System.currentTimeMillis())
            return
        }

        val seen = HashSet<String>()
        val existing = db.fileDao().forFolder(folder.id).associateBy { it.uri }.toMutableMap()
        scanDirectory(db, folder, tree, "", seen, existing)

        val rows = db.fileDao().forFolder(folder.id)
        for (row in rows) {
            if (row.uri !in seen) db.fileDao().delete(row.id)
        }

        val pathIds = HashMap<String, String>()
        pathIds[""] = remoteRootId
        val pending = rows
            .filter { it.uri in seen }
            .filter { it.status == FileStatus.PENDING || it.status == FileStatus.FAILED }
            .sortedBy { it.parentPath }

        for (row in pending) {
            currentCoroutineContext().ensureActive()
            val remoteFolderId = try {
                ensureRemotePath(client, remoteRootId, row.parentPath, pathIds, cache)
            } catch (e: CancellationException) {
                throw e
            } catch (e: PermanentItemException) {
                db.fileDao().markFailed(row.id, FileStatus.SKIPPED, e.message ?: "invalid path")
                continue
            } catch (e: NimboApiException) {
                if (e.code == 404) {
                    resetRemoteStructure(db, folder.id)
                    outcome.transientFailures = true
                    break
                }
                if (recordFailure(db, row, e)) outcome.transientFailures = true
                continue
            }
            val transient = try {
                uploadRow(
                    context = context,
                    client = client,
                    db = db,
                    row = row,
                    remoteFolderId = remoteFolderId,
                    maxUploadBytes = maxUploadBytes,
                    cache = cache,
                )
            } catch (e: CancellationException) {
                throw e
            } catch (e: NimboAuthException) {
                throw e
            } catch (e: RemoteStructureMissing) {
                resetRemoteStructure(db, folder.id)
                outcome.transientFailures = true
                break
            } catch (e: Exception) {
                val failedTransient = recordFailure(db, row, e)
                if (failedTransient) outcome.transientFailures = true
                failedTransient
            }
            if (transient) outcome.transientFailures = true
        }

        db.folderDao().setLastScan(folder.id, System.currentTimeMillis())
    }

    private data class RemoteIdentity(val origin: String, val accountId: String?)

    /**
     * Returns the cached remote root id/path only when it belongs to the same
     * normalized server origin and account. Otherwise cached node ids are
     * cleared so a re-pair cannot address another server's UUIDs.
     */
    private suspend fun reconcileRemoteIdentity(
        db: AppDb,
        folder: FolderEntity,
        identity: RemoteIdentity,
    ): Pair<String?, String?> {
        val hasCache = folder.remoteFolderId != null
        val originChanged = folder.remoteOrigin == null || folder.remoteOrigin != identity.origin
        val accountChanged = hasCache && identity.accountId != null &&
            folder.remoteAccount != identity.accountId
        val reset = originChanged || accountChanged

        if (reset) {
            db.folderDao().clearRemote(folder.id)
            db.fileDao().clearRemoteIdsForFolder(folder.id)
        }
        db.folderDao().setRemote(
            folder.id,
            if (reset) null else folder.remoteFolderId,
            if (reset) null else folder.remotePath,
            identity.origin,
            identity.accountId ?: folder.remoteAccount,
        )
        return if (reset) null to null else folder.remoteFolderId to folder.remotePath
    }

    private suspend fun resetRemoteStructure(db: AppDb, folderId: Long) {
        db.folderDao().clearRemote(folderId)
        db.fileDao().clearRemoteIdsForFolder(folderId)
    }

    /** Depth-first scan of the SAF tree; upserts file rows and schedules work. */
    private suspend fun scanDirectory(
        db: AppDb,
        folder: FolderEntity,
        dir: DocumentFile,
        relPath: String,
        seen: MutableSet<String>,
        existing: MutableMap<String, FileEntity>,
    ) {
        currentCoroutineContext().ensureActive()
        val children = dir.listFiles() ?: return
        for (doc in children) {
            currentCoroutineContext().ensureActive()
            if (!doc.exists()) continue
            if (doc.isDirectory) {
                val childPath = joinPath(relPath, doc.name.orEmpty())
                scanDirectory(db, folder, doc, childPath, seen, existing)
                continue
            }
            val rawName = doc.name ?: continue
            val name = RemoteNameRules.normalize(rawName) ?: rawName
            val uri = doc.uri.toString()
            seen.add(uri)
            val size = doc.length()
            val mtime = doc.lastModified()
            val row = existing[uri]
            if (row == null) {
                val entity = FileEntity(
                    folderId = folder.id,
                    uri = uri,
                    parentPath = relPath,
                    name = name,
                    size = size,
                    mtime = mtime,
                    sha256 = null,
                    status = FileStatus.PENDING,
                    remoteNodeId = null,
                    remoteName = null,
                    contentDirty = true,
                    moveDirty = false,
                )
                val id = db.fileDao().insert(entity)
                if (id > 0) {
                    existing[uri] = entity.copy(id = id)
                } else {
                    db.fileDao().byUri(folder.id, uri)?.let { existing[uri] = it }
                }
                continue
            }

            val contentChanged = row.size != size || row.mtime != mtime
            val locationChanged = row.name != name || row.parentPath != relPath
            if (!contentChanged && !locationChanged) continue

            val contentDirty = contentChanged || row.status == FileStatus.SKIPPED || row.contentDirty
            val moveDirty = locationChanged || row.moveDirty
            val sha = if (contentChanged) null else row.sha256
            db.fileDao().markChanged(
                id = row.id,
                size = size,
                mtime = mtime,
                name = name,
                parentPath = relPath,
                status = FileStatus.PENDING,
                sha = sha,
                contentDirty = contentDirty,
                moveDirty = moveDirty,
                attempts = 0,
            )
            existing[uri] = row.copy(
                size = size,
                mtime = mtime,
                name = name,
                parentPath = relPath,
                status = FileStatus.PENDING,
                sha256 = sha,
                contentDirty = contentDirty,
                moveDirty = moveDirty,
                attempts = 0,
                lastError = null,
            )
        }
    }

    private suspend fun uploadRow(
        context: Context,
        client: NimboClient,
        db: AppDb,
        row: FileEntity,
        remoteFolderId: String,
        maxUploadBytes: Long?,
        cache: RemoteCache,
    ): Boolean {
        val name = RemoteNameRules.normalize(row.name)
        if (name == null) {
            db.fileDao().markFailed(
                row.id,
                FileStatus.SKIPPED,
                "file name cannot be mirrored (blank, control characters, or over 255 bytes)",
            )
            return false
        }
        if (row.size <= 0) {
            db.fileDao().markFailed(row.id, FileStatus.SKIPPED, "empty files cannot be backed up")
            return false
        }
        if (maxUploadBytes != null && row.size > maxUploadBytes) {
            db.fileDao().markFailed(
                row.id,
                FileStatus.SKIPPED,
                "file is larger than the server upload limit (${maxUploadBytes / (1024 * 1024)} MiB)",
            )
            return false
        }

        var remoteId = row.remoteNodeId
        var spooled: Spooled? = null
        try {
            // Metadata-only reconciliation: a stable local URI changed name/path.
            if (remoteId != null && !row.contentDirty) {
                try {
                    return applyMove(client, db, row, remoteId, name, remoteFolderId, cache)
                } catch (e: NotFoundException) {
                    db.fileDao().clearRemoteId(row.id)
                    remoteId = null
                    SafeLog.d(TAG, "remote node disappeared; re-uploading")
                }
            }

            if (remoteId != null) {
                val content = spool(context, row)
                spooled = content
                try {
                    val node = client.replaceContent(remoteId, name, content.file)
                    requireVerified(node, content.sha)
                    cache.removeNodeEverywhere(node.id)

                    if (row.moveDirty) {
                        val moved = try {
                            client.updateNode(node.id, name, remoteFolderId)
                        } catch (e: CancellationException) {
                            throw e
                        } catch (e: NimboAuthException) {
                            throw e
                        } catch (e: NimboApiException) {
                            if (e.code == 404) throw NotFoundException()
                            return recordMoveFailure(db, row, node, content.sha, e)
                        } catch (e: Exception) {
                            return recordMoveFailure(db, row, node, content.sha, e)
                        }
                        requireSameContent(moved, content.sha)
                        cache.removeNodeEverywhere(moved.id)
                        cache.record(remoteFolderId, moved)
                        db.fileDao().markUploaded(
                            row.id, content.sha, moved.id, moved.name,
                            System.currentTimeMillis(), FileStatus.UPLOADED,
                        )
                    } else {
                        cache.record(remoteFolderId, node)
                        db.fileDao().markUploaded(
                            row.id, content.sha, node.id, node.name,
                            System.currentTimeMillis(), FileStatus.UPLOADED,
                        )
                    }
                    return false
                } catch (e: NotFoundException) {
                    db.fileDao().clearRemoteId(row.id)
                    remoteId = null
                } catch (e: NimboApiException) {
                    if (e.code != 404) throw e
                    db.fileDao().clearRemoteId(row.id)
                    remoteId = null
                }
            }

            // New upload or recovery after the tracked remote node disappeared.
            val content = spooled ?: spool(context, row).also { spooled = it }
            val candidate = cache.items(remoteFolderId).firstOrNull { item ->
                ContentIdentity.isAdoptable(
                    RemoteCandidate(item.id, item.kind, item.name, item.size, item.sha256),
                    content.sha,
                    content.size,
                )
            }
            if (candidate != null) {
                db.fileDao().markUploaded(
                    row.id,
                    content.sha,
                    candidate.id,
                    candidate.name,
                    System.currentTimeMillis(),
                    FileStatus.UPLOADED,
                )
                cache.record(remoteFolderId, candidate)
                return false
            }

            val node = client.upload(remoteFolderId, name, content.file)
            requireVerified(node, content.sha)
            cache.record(remoteFolderId, node)
            db.fileDao().markUploaded(
                row.id, content.sha, node.id, node.name,
                System.currentTimeMillis(), FileStatus.UPLOADED,
            )
            return false
        } catch (e: CancellationException) {
            throw e
        } catch (e: NimboAuthException) {
            throw e
        } catch (e: NimboApiException) {
            if (e.code == 404) throw RemoteStructureMissing()
            return recordFailure(db, row, e)
        } catch (e: Exception) {
            return recordFailure(db, row, e)
        } finally {
            spooled?.file?.delete()
        }
    }

    /** Applies a rename/move to an already-uploaded node without touching bytes. */
    private suspend fun applyMove(
        client: NimboClient,
        db: AppDb,
        row: FileEntity,
        remoteId: String,
        name: String,
        remoteFolderId: String,
        cache: RemoteCache,
    ): Boolean {
        val node = try {
            client.updateNode(remoteId, name, remoteFolderId)
        } catch (e: NimboApiException) {
            if (e.code == 404) throw NotFoundException()
            throw e
        }
        if (node.sha256 != null && row.sha256 != null &&
            !ContentIdentity.hashesEqual(node.sha256, row.sha256)
        ) {
            throw VerificationException("server content hash changed during move")
        }
        if (node.kind != "file" || node.id.isBlank()) {
            throw VerificationException("server returned an invalid file entry")
        }
        cache.removeNodeEverywhere(node.id)
        cache.record(remoteFolderId, node)
        db.fileDao().markUploaded(
            row.id,
            row.sha256 ?: node.sha256,
            node.id,
            node.name,
            System.currentTimeMillis(),
            FileStatus.UPLOADED,
        )
        return false
    }

    private fun requireVerified(node: NodeSummary, localSha: String) {
        if (node.kind != "file" || node.id.isBlank()) {
            throw VerificationException("server returned an invalid file entry")
        }
        if (!ContentIdentity.hashesEqual(node.sha256, localSha)) {
            throw VerificationException("server did not confirm the uploaded content hash")
        }
    }

    private fun requireSameContent(node: NodeSummary, localSha: String) {
        if (node.kind != "file" || node.id.isBlank()) {
            throw VerificationException("server returned an invalid file entry")
        }
        if (node.sha256 != null && !ContentIdentity.hashesEqual(node.sha256, localSha)) {
            throw VerificationException("server content hash changed during move")
        }
    }

    private suspend fun recordFailure(db: AppDb, row: FileEntity, error: Exception): Boolean {
        if (error is NimboAuthException) throw error
        val kind = when (error) {
            is VerificationException -> FailureKind.PERMANENT
            is NimboApiException -> RetryPolicy.classifyHttp(error.code)
            is SecurityException -> FailureKind.TRANSIENT
            is IOException -> FailureKind.TRANSIENT
            else -> FailureKind.TRANSIENT
        }
        val nextAttempts = row.attempts + 1
        val permanent = kind == FailureKind.PERMANENT || nextAttempts >= RetryPolicy.MAX_ATTEMPTS
        val status = if (permanent) FileStatus.SKIPPED else FileStatus.FAILED
        db.fileDao().markFailed(row.id, status, describe(error))
        return !permanent
    }

    /**
     * Content replacement already committed, but the follow-up rename/move was
     * rejected. Persist the verified content hash/id so a retry only performs
     * the move instead of re-spooling the whole file.
     */
    private suspend fun recordMoveFailure(
        db: AppDb,
        row: FileEntity,
        node: NodeSummary,
        sha: String,
        error: Exception,
    ): Boolean {
        val kind = when (error) {
            is VerificationException -> FailureKind.PERMANENT
            is NimboApiException -> RetryPolicy.classifyHttp(error.code)
            else -> FailureKind.TRANSIENT
        }
        val nextAttempts = row.attempts + 1
        val permanent = kind == FailureKind.PERMANENT || nextAttempts >= RetryPolicy.MAX_ATTEMPTS
        val status = if (permanent) FileStatus.SKIPPED else FileStatus.FAILED
        db.fileDao().markContentSynced(
            row.id, sha, node.id, node.name, status, describe(error),
        )
        return !permanent
    }

    private fun describe(error: Throwable): String {
        val message = error.message?.trim()?.take(240)
        return if (message.isNullOrBlank()) "backup failed" else message
    }

    private suspend fun ensureRemoteFolder(
        client: NimboClient,
        db: AppDb,
        folderId: Long,
        cachedId: String?,
        cachedPath: String?,
        deviceName: String,
        displayName: String,
        cache: RemoteCache,
        identity: RemoteIdentity,
    ): String {
        val segments = listOf(ROOT_PREFIX, deviceName, displayName).map {
            RemoteNameRules.normalize(it)
                ?: throw PermanentItemException("folder name cannot be mirrored")
        }
        val path = segments.joinToString("/")
        if (cachedId != null && cachedPath == path) return cachedId

        if (cachedId != null && cachedPath != null) {
            try {
                var parentId = ""
                for (segment in segments.dropLast(1)) {
                    parentId = cache.ensureFolder(parentId, segment)
                }
                val node = client.updateNode(cachedId, segments.last(), parentId)
                if (node.kind != "folder") {
                    throw PermanentItemException("server returned an invalid folder entry")
                }
                cache.removeNodeEverywhere(cachedId)
                cache.record(parentId, node)
                db.folderDao().setRemote(folderId, node.id, path, identity.origin, identity.accountId)
                return node.id
            } catch (e: CancellationException) {
                throw e
            } catch (e: NimboAuthException) {
                throw e
            } catch (e: NimboApiException) {
                // 404/409: move is not possible (node gone or target name taken).
                // Fall back to a fresh mirror and re-upload tracked files there.
                if (e.code != 404 && e.code != 409) throw e
                db.fileDao().clearRemoteIdsForFolder(folderId)
                db.folderDao().clearRemote(folderId)
            }
        } else if (cachedId != null) {
            db.fileDao().clearRemoteIdsForFolder(folderId)
            db.folderDao().clearRemote(folderId)
        }

        var parentId = ""
        for (segment in segments) {
            parentId = cache.ensureFolder(parentId, segment)
        }
        db.folderDao().setRemote(folderId, parentId, path, identity.origin, identity.accountId)
        return parentId
    }

    /** Resolve (or create) a nested remote folder for a relative local path. */
    private suspend fun ensureRemotePath(
        client: NimboClient,
        remoteRootId: String,
        relPath: String,
        cache: MutableMap<String, String>,
        remote: RemoteCache,
    ): String {
        val normalized = RemoteNameRules.normalizePath(relPath)
            ?: throw PermanentItemException("folder path cannot be mirrored")
        if (normalized.isEmpty()) return remoteRootId
        cache[normalized]?.let { return it }
        val segments = normalized.split('/')
        val parentRel = segments.dropLast(1).joinToString("/")
        val parentId = ensureRemotePath(client, remoteRootId, parentRel, cache, remote)
        val id = remote.ensureFolder(parentId, segments.last())
        cache[normalized] = id
        return id
    }

    private class RemoteCache(private val client: NimboClient) {
        private val listings = HashMap<String, MutableList<NodeSummary>>()

        suspend fun items(folderId: String): List<NodeSummary> =
            listings.getOrPut(folderId) { client.listFolder(folderId).items.toMutableList() }

        suspend fun ensureFolder(parentId: String, name: String): String {
            items(parentId).firstOrNull {
                it.kind == "folder" && RemoteNameRules.same(it.name, name)
            }?.let { return it.id }
            return try {
                val node = client.createFolder(parentId, name)
                record(parentId, node)
                node.id
            } catch (e: NimboApiException) {
                if (e.code != 409) throw e
                // Another device may have created the same case-folded sibling.
                listings.remove(parentId)
                items(parentId).firstOrNull {
                    it.kind == "folder" && RemoteNameRules.same(it.name, name)
                }?.id ?: throw e
            }
        }

        fun record(parentId: String, node: NodeSummary) {
            val list = listings.getOrPut(parentId) { mutableListOf() }
            list.removeAll { it.id == node.id }
            list.add(node)
        }

        fun removeNodeEverywhere(nodeId: String) {
            listings.values.forEach { list -> list.removeAll { it.id == nodeId } }
        }
    }

    private data class Spooled(val file: File, val sha: String, val size: Long)

    /** Stream the document to a temp file, hashing as we go. Never mutates the server. */
    private suspend fun spool(context: Context, row: FileEntity): Spooled {
        val free = context.cacheDir.usableSpace
        if (row.size > 0 && free in 1 until (row.size + CACHE_MARGIN_BYTES)) {
            throw IOException("not enough cache space to stage this file")
        }
        val temp = File.createTempFile("nimbo-up-", ".part", context.cacheDir)
        try {
            val stream: InputStream = context.contentResolver.openInputStream(Uri.parse(row.uri))
                ?: throw IOException("cannot open the selected file")
            val digest = MessageDigest.getInstance("SHA-256")
            var total = 0L
            stream.use { input ->
                temp.outputStream().use { output ->
                    val buffer = ByteArray(256 * 1024)
                    while (true) {
                        currentCoroutineContext().ensureActive()
                        val read = input.read(buffer)
                        if (read < 0) break
                        // Enforce the scanned size while streaming: a provider that
                        // grows the file between scan and spool must not fill cache.
                        if (StreamSizeGuard.wouldExceed(total, read, row.size)) {
                            throw IOException("file grew during backup; retrying later")
                        }
                        digest.update(buffer, 0, read)
                        output.write(buffer, 0, read)
                        total += read
                    }
                }
            }
            if (total != row.size) throw IOException("file changed during backup; retrying later")
            return Spooled(temp, hex(digest.digest()), total)
        } catch (e: Exception) {
            temp.delete()
            throw e
        }
    }

    private fun cleanStaleParts(context: Context) {
        context.cacheDir.listFiles { file ->
            file.name.startsWith("nimbo-up-") && file.name.endsWith(".part")
        }?.forEach { it.delete() }
    }

    private fun joinPath(parent: String, name: String): String =
        if (parent.isEmpty()) name else "$parent/$name"

    private fun hex(bytes: ByteArray): String =
        bytes.joinToString("") { "%02x".format(it) }

    private class PermanentItemException(message: String) : Exception(message)
    private class VerificationException(message: String) : IOException(message)
    private class NotFoundException : Exception()
    private class RemoteStructureMissing : Exception()
}
