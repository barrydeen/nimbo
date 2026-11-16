package app.nimbo.backup.data.db

import android.content.Context
import androidx.room.Dao
import androidx.room.Database
import androidx.room.Entity
import androidx.room.Index
import androidx.room.Insert
import androidx.room.OnConflictStrategy
import androidx.room.PrimaryKey
import androidx.room.Query
import androidx.room.Room
import androidx.room.RoomDatabase
import kotlinx.coroutines.flow.Flow

@Entity(tableName = "folders", indices = [Index(value = ["treeUri"], unique = true)])
data class FolderEntity(
    @PrimaryKey(autoGenerate = true) val id: Long = 0,
    /** SAF tree URI string (content://.../tree/...). */
    val treeUri: String,
    /** Local display name (last path segment of the tree). */
    val displayName: String,
    val enabled: Boolean = true,
    /** Remote folder id on the server for `Phone Backups/<device>/<name>`, when resolved. */
    val remoteFolderId: String? = null,
    val remotePath: String? = null,
    val lastScanAt: Long? = null,
    val createdAt: Long,
)

enum class FileStatus { PENDING, UPLOADED, FAILED }

@Entity(
    tableName = "files",
    indices = [Index(value = ["folderId", "uri"], unique = true)],
)
data class FileEntity(
    @PrimaryKey(autoGenerate = true) val id: Long = 0,
    val folderId: Long,
    /** Document file URI (content://.../document/...). Stable per document id. */
    val uri: String,
    /** Path of the containing directory relative to the tree root, "" for the root. */
    val parentPath: String,
    val name: String,
    val size: Long,
    val mtime: Long,
    val sha256: String? = null,
    val status: FileStatus = FileStatus.PENDING,
    val remoteNodeId: String? = null,
    val attempts: Int = 0,
    val lastError: String? = null,
    val uploadedAt: Long? = null,
)

data class FolderCounts(
    val folderId: Long,
    val uploaded: Int,
    val pending: Int,
    val failed: Int,
)

@Dao
interface FolderDao {
    @Query("SELECT * FROM folders ORDER BY createdAt")
    fun observeAll(): Flow<List<FolderEntity>>

    @Query("SELECT * FROM folders ORDER BY createdAt")
    suspend fun all(): List<FolderEntity>

    @Query("SELECT * FROM folders WHERE enabled = 1")
    suspend fun enabled(): List<FolderEntity>

    @Query("SELECT * FROM folders WHERE treeUri = :treeUri LIMIT 1")
    suspend fun byTreeUri(treeUri: String): FolderEntity?

    @Query("SELECT * FROM folders WHERE id = :id LIMIT 1")
    suspend fun byId(id: Long): FolderEntity?

    @Insert(onConflict = OnConflictStrategy.IGNORE)
    suspend fun insert(folder: FolderEntity): Long

    @Query("UPDATE folders SET remoteFolderId = :remoteId, remotePath = :remotePath WHERE id = :id")
    suspend fun setRemote(id: Long, remoteId: String?, remotePath: String?)

    @Query("UPDATE folders SET enabled = :enabled WHERE id = :id")
    suspend fun setEnabled(id: Long, enabled: Boolean)

    @Query("UPDATE folders SET lastScanAt = :at WHERE id = :id")
    suspend fun setLastScan(id: Long, at: Long)

    @Query("DELETE FROM folders WHERE id = :id")
    suspend fun delete(id: Long)
}

@Dao
interface FileDao {
    @Query("SELECT * FROM files WHERE folderId = :folderId")
    suspend fun forFolder(folderId: Long): List<FileEntity>

    @Query("SELECT * FROM files WHERE folderId = :folderId AND status = :status")
    suspend fun forFolderWithStatus(folderId: Long, status: FileStatus): List<FileEntity>

    @Query("SELECT * FROM files WHERE uri = :uri LIMIT 1")
    suspend fun byUri(uri: String): FileEntity?

    @Query("SELECT COUNT(*) FROM files WHERE folderId = :folderId AND status = :status")
    suspend fun countStatus(folderId: Long, status: FileStatus): Int

    @Insert(onConflict = OnConflictStrategy.IGNORE)
    suspend fun insert(file: FileEntity): Long

    @Query("SELECT * FROM files WHERE folderId = :folderId AND parentPath = :parentPath AND name = :name LIMIT 1")
    suspend fun byRelativePath(folderId: Long, parentPath: String, name: String): FileEntity?

    @Query(
        "UPDATE files SET status = :status, sha256 = :sha, remoteNodeId = :remoteNodeId, " +
            "uploadedAt = :uploadedAt, attempts = 0, lastError = NULL WHERE id = :id",
    )
    suspend fun markUploaded(id: Long, sha: String?, remoteNodeId: String?, uploadedAt: Long, status: FileStatus)

    @Query(
        "UPDATE files SET status = :status, attempts = attempts + 1, lastError = :error WHERE id = :id",
    )
    suspend fun markFailed(id: Long, status: FileStatus, error: String)

    @Query(
        "UPDATE files SET size = :size, mtime = :mtime, status = :status, sha256 = :sha, " +
            "remoteNodeId = :remoteNodeId, uploadedAt = NULL WHERE id = :id",
    )
    suspend fun markChanged(id: Long, size: Long, mtime: Long, status: FileStatus, sha: String?, remoteNodeId: String?)

    @Query("DELETE FROM files WHERE id = :id")
    suspend fun delete(id: Long)

    @Query("DELETE FROM files WHERE folderId = :folderId")
    suspend fun deleteAllForFolder(folderId: Long)

    @Query(
        "SELECT folderId, " +
            "SUM(CASE WHEN status = 'UPLOADED' THEN 1 ELSE 0 END) AS uploaded, " +
            "SUM(CASE WHEN status = 'PENDING' THEN 1 ELSE 0 END) AS pending, " +
            "SUM(CASE WHEN status = 'FAILED' THEN 1 ELSE 0 END) AS failed " +
            "FROM files GROUP BY folderId",
    )
    fun observeCounts(): Flow<List<FolderCounts>>

    @Query("SELECT COALESCE(SUM(size), 0) FROM files WHERE status = 'UPLOADED'")
    fun observeTotalUploaded(): Flow<Long>
}

@Database(
    entities = [FolderEntity::class, FileEntity::class],
    version = 1,
    exportSchema = false,
)
abstract class AppDb : RoomDatabase() {
    abstract fun folderDao(): FolderDao
    abstract fun fileDao(): FileDao

    companion object {
        @Volatile private var instance: AppDb? = null

        fun get(context: Context): AppDb = instance ?: synchronized(this) {
            instance ?: build(context).also { instance = it }
        }

        private fun build(context: Context): AppDb =
            Room.databaseBuilder(context, AppDb::class.java, "nimbo-backup.db")
                .fallbackToDestructiveMigration(false)
                .build()
    }
}
