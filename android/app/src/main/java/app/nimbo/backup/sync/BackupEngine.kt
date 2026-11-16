package app.nimbo.backup.sync

import android.content.Context
import android.net.Uri
import android.util.Log
import androidx.documentfile.provider.DocumentFile
import app.nimbo.backup.data.Prefs
import app.nimbo.backup.data.api.NimboAuthException
import app.nimbo.backup.data.api.NimboClient
import app.nimbo.backup.data.db.AppDb
import app.nimbo.backup.data.db.FileEntity
import app.nimbo.backup.data.db.FileStatus
import app.nimbo.backup.data.db.FolderEntity
import app.nimbo.backup.data.isConfigured
import kotlinx.coroutines.flow.firstOrNull
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.security.MessageDigest

/**
 * One-way phone → Nimbo backup engine.
 *
 * Per enabled folder:
 *  1. Walk the SAF tree, reconciling every document against the local Room state.
 *  2. Mirror the folder's directory structure on the server under
 *     `Phone Backups/<device name>/<local folder name>/...`.
 *  3. Upload pending files (idempotent: skipped when the remote folder already holds a
 *     file with the same name and size, e.g. after a reinstall).
 *
 * Files deleted on the phone keep their server copy; content changes replace the remote
 * node so the server never accumulates "name (2)" duplicates.
 */
object BackupEngine {

    private const val TAG = "NimboBackup"
    private const val ROOT_PREFIX = "Phone Backups"

    suspend fun run(context: Context): Outcome {
        val prefs = Prefs(context)
        val config = prefs.config.firstOrNull() ?: return Outcome.NotConfigured
        if (!config.isConfigured) return Outcome.NotConfigured

        val db = AppDb.get(context)
        val client = NimboClient(config.serverUrl, config.token)
        val outcome = Outcome.Ok()

        for (folder in db.folderDao().enabled()) {
            try {
                Log.d(TAG, "syncing folder ${folder.displayName} (${folder.treeUri})")
                syncFolder(context, client, db, folder, config.deviceName)
                Log.d(TAG, "sync finished for ${folder.displayName}")
            } catch (e: NimboAuthException) {
                Log.e(TAG, "auth rejected for ${folder.displayName}", e)
                prefs.markAuthFailed()
                outcome.errors += "Server rejected the device token — re-pair in the app."
                break
            } catch (e: IOException) {
                Log.e(TAG, "io error for ${folder.displayName}", e)
                outcome.errors += "${folder.displayName}: ${e.message ?: "network error"}"
            } catch (e: Exception) {
                Log.e(TAG, "unexpected error for ${folder.displayName}", e)
                outcome.errors += "${folder.displayName}: ${e.message ?: "unexpected error"}"
            }
        }
        return outcome
    }

    sealed interface Outcome {
        data object NotConfigured : Outcome
        data class Ok(var errors: MutableList<String> = mutableListOf()) : Outcome
    }

    private suspend fun syncFolder(
        context: Context,
        client: NimboClient,
        db: AppDb,
        folder: FolderEntity,
        deviceName: String,
    ) {
        val tree = DocumentFile.fromTreeUri(context, Uri.parse(folder.treeUri)) ?: run {
            db.folderDao().setEnabled(folder.id, false)
            throw IOException("folder permission lost — re-add it in the app")
        }
        if (!tree.exists()) {
            db.folderDao().setEnabled(folder.id, false)
            throw IOException("folder no longer exists on the phone")
        }

        // 1. Scan: reconcile documents with local state.
        val seen = HashSet<String>()
        scanDirectory(db, folder, tree, "") { uri -> seen.add(uri) }

        // Drop rows whose documents vanished (server copies are kept).
        val stale = db.fileDao().forFolder(folder.id).filter { it.uri !in seen }
        for (row in stale) db.fileDao().delete(row.id)

        // 2. Ensure the remote mirror: root + track path.
        val remoteRootId = ensureRemoteFolder(client, db, folder, deviceName, tree.name ?: folder.displayName)
        val remoteIdCache = HashMap<String, String>()
        remoteIdCache[""] = remoteRootId

        // 3. Upload pendings and failures (fresh attempts each run).
        val pending = db.fileDao().forFolder(folder.id).filter {
            it.status == FileStatus.PENDING || it.status == FileStatus.FAILED
        }.sortedBy { it.parentPath }

        for (row in pending) {
            val remoteFolderId = ensureRemotePath(client, db, folder, remoteRootId, row.parentPath, remoteIdCache)
            uploadRow(context, client, db, row, remoteFolderId)
        }

        db.folderDao().setLastScan(folder.id, System.currentTimeMillis())
    }

    /** Depth-first scan of the SAF tree; upserts file rows and schedules re-uploads. */
    private suspend fun scanDirectory(
        db: AppDb,
        folder: FolderEntity,
        dir: DocumentFile,
        relPath: String,
        onSeen: (String) -> Unit,
    ) {
        val children = dir.listFiles() ?: return
        for (doc in children) {
            if (!doc.exists()) continue
            if (doc.isDirectory) {
                val childPath = if (relPath.isEmpty()) doc.name.orEmpty() else "$relPath/${doc.name.orEmpty()}"
                scanDirectory(db, folder, doc, childPath, onSeen)
                continue
            }
            val name = doc.name ?: continue
            val uri = doc.uri.toString()
            onSeen(uri)
            val size = doc.length()
            val mtime = doc.lastModified()
            val existing = db.fileDao().byUri(uri)
            when {
                existing == null -> {
                    // New file for us — record it, dedupe races via the unique index.
                    db.fileDao().insert(
                        FileEntity(
                            folderId = folder.id,
                            uri = uri,
                            parentPath = relPath,
                            name = name,
                            size = size,
                            mtime = mtime,
                            sha256 = null,
                            status = FileStatus.PENDING,
                        ),
                    )
                }
                existing.status == FileStatus.UPLOADED &&
                    (existing.size != size || existing.mtime != mtime) -> {
                    // Content changed after upload — the remote copy is replaced during
                    // upload so we don't accumulate "photo (2).jpg" entries.
                    db.fileDao().markChanged(
                        existing.id, size, mtime,
                        FileStatus.PENDING, sha = null, remoteNodeId = existing.remoteNodeId,
                    )
                }
            }
        }
    }

    /** Resolve (or create) `Phone Backups/<device>/<folder name>`, caching the id. */
    private suspend fun ensureRemoteFolder(
        client: NimboClient,
        db: AppDb,
        folder: FolderEntity,
        deviceName: String,
        displayName: String,
    ): String {
        folder.remoteFolderId?.let { if (folder.remotePath == "$ROOT_PREFIX/$deviceName/$displayName") return it }

        var parentId = ""
        for (segment in listOf(ROOT_PREFIX, deviceName, displayName)) {
            parentId = ensureChildFolder(client, parentId, segment)
        }
        val path = "$ROOT_PREFIX/$deviceName/$displayName"
        db.folderDao().setRemote(folder.id, parentId, path)
        return parentId
    }

    /** Resolve (or create) a nested remote folder for a relative local path. */
    private suspend fun ensureRemotePath(
        client: NimboClient,
        db: AppDb,
        folder: FolderEntity,
        remoteRootId: String,
        relPath: String,
        cache: MutableMap<String, String>,
    ): String {
        if (relPath.isBlank()) return remoteRootId
        cache[relPath]?.let { return it }
        val segments = relPath.split('/')
        // Resolve the parent first so intermediate folders get created too.
        val parentRel = segments.dropLast(1).joinToString("/")
        val parentId = ensureRemotePath(client, db, folder, remoteRootId, parentRel, cache)
        val id = ensureChildFolder(client, parentId, segments.last())
        cache[relPath] = id
        return id
    }

    /** Find a child folder by name; create it when missing. */
    private fun ensureChildFolder(client: NimboClient, parentId: String, name: String): String {
        val listing = client.listFolder(parentId)
        listing.items.firstOrNull { it.kind == "folder" && it.name == name }?.let { return it.id }
        return client.createFolder(parentId, name).id
    }

    /** Hash + upload one row. Skips when the server already has this name+size. */
    private suspend fun uploadRow(
        context: Context,
        client: NimboClient,
        db: AppDb,
        row: FileEntity,
        remoteFolderId: String,
    ) {
        val doc = DocumentFile.fromSingleUri(context, android.net.Uri.parse(row.uri))
        if (doc == null || !doc.exists() || doc.length() != row.size) {
            if (doc?.exists() == true && doc.length() != row.size) {
                // size drifted between scan and upload; refresh and let next run retry
                db.fileDao().markChanged(row.id, doc.length(), doc.lastModified(), FileStatus.PENDING, null, null)
                return
            }
            db.fileDao().delete(row.id)
            return
        }

        // Idempotency: if the server folder already holds this name with this size, adopt it.
        val remote = client.listFolder(remoteFolderId)
        val existing = remote.items.firstOrNull { it.name == row.name && it.size == row.size }
        if (existing != null) {
            val sha = row.sha256 ?: hashDocument(context, row)?.second
            db.fileDao().markUploaded(row.id, sha, existing.id, System.currentTimeMillis(), FileStatus.UPLOADED)
            return
        }

        // Replace a changed remote copy before uploading the new content.
        row.remoteNodeId?.let { runCatching { client.deleteNode(it) } }

        val (sha, tempFile) = try {
            spoolToTemp(context, row)
        } catch (e: IOException) {
            db.fileDao().markFailed(row.id, FileStatus.FAILED, e.message ?: "read error")
            return
        }

        try {
            val node = client.upload(remoteFolderId, row.name, tempFile)
            db.fileDao().markUploaded(row.id, sha, node.id, System.currentTimeMillis(), FileStatus.UPLOADED)
        } catch (e: NimboAuthException) {
            db.fileDao().markFailed(row.id, FileStatus.FAILED, e.message ?: "auth error")
            throw e
        } catch (e: IOException) {
            db.fileDao().markFailed(row.id, FileStatus.FAILED, e.message ?: "upload error")
        } finally {
            tempFile.delete()
        }
    }

    /** Stream the document to a temp file, hashing as we go. */
    private fun spoolToTemp(context: Context, row: FileEntity): Pair<String, File> {
        val resolver = context.contentResolver
        val stream: InputStream = resolver.openInputStream(android.net.Uri.parse(row.uri))
            ?: throw IOException("cannot open ${row.name}")
        val digest = MessageDigest.getInstance("SHA-256")
        val temp = File.createTempFile("nimbo-up-", ".part", context.cacheDir)
        try {
            stream.use { input ->
                temp.outputStream().use { output ->
                    val buf = ByteArray(256 * 1024)
                    var total = 0L
                    while (true) {
                        val n = input.read(buf)
                        if (n < 0) break
                        digest.update(buf, 0, n)
                        output.write(buf, 0, n)
                        total += n
                    }
                    if (total != row.size && row.size > 0) {
                        throw IOException("${row.name}: size changed during upload")
                    }
                }
            }
        } catch (e: Exception) {
            temp.delete()
            throw e
        }
        val sha = hex(digest.digest())
        return sha to temp
    }

    /** Best-effort hash used only for the skip-if-uploaded shortcut. */
    private fun hashDocument(context: Context, row: FileEntity): Pair<Long, String>? {
        return try {
            val resolver = context.contentResolver
            val digest = MessageDigest.getInstance("SHA-256")
            var size = 0L
            val input = resolver.openInputStream(android.net.Uri.parse(row.uri))
            if (input == null) return null
            input.use {
                val buf = ByteArray(256 * 1024)
                while (true) {
                    val n = it.read(buf)
                    if (n < 0) break
                    digest.update(buf, 0, n)
                    size += n
                }
            }
            size to hex(digest.digest())
        } catch (_: Exception) {
            null
        }
    }

    private fun hex(bytes: ByteArray): String =
        bytes.joinToString("") { "%02x".format(it) }
}
