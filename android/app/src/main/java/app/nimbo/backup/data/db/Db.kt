package app.nimbo.backup.data.db

import android.content.Context
import androidx.room.Dao
import androidx.room.Database
import androidx.room.Entity
import androidx.room.ForeignKey
import androidx.room.Index
import androidx.room.Insert
import androidx.room.OnConflictStrategy
import androidx.room.PrimaryKey
import androidx.room.Query
import androidx.room.Room
import androidx.room.RoomDatabase
import androidx.room.Transaction
import androidx.room.migration.Migration
import androidx.sqlite.db.SupportSQLiteDatabase
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
    /** Normalized scheme://host:port that [remoteFolderId] belongs to. */
    val remoteOrigin: String? = null,
    /** Authenticated server account id that [remoteFolderId] belongs to. */
    val remoteAccount: String? = null,
    val lastScanAt: Long? = null,
    val createdAt: Long,
)

enum class FileStatus { PENDING, UPLOADED, FAILED, SKIPPED }

@Entity(
    tableName = "files",
    foreignKeys = [
        ForeignKey(
            entity = FolderEntity::class,
            parentColumns = ["id"],
            childColumns = ["folderId"],
            onDelete = ForeignKey.CASCADE,
        ),
    ],
    indices = [
        Index(value = ["folderId", "uri"], unique = true),
        Index(value = ["folderId"]),
    ],
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
    /** Name returned by the server; may differ after auto-suffixing. */
    val remoteName: String? = null,
    /** Content differs from the remote node and needs replacement/upload. */
    val contentDirty: Boolean = true,
    /** Name/parent differs from the remote node and needs a PATCH move. */
    val moveDirty: Boolean = false,
    val attempts: Int = 0,
    val lastError: String? = null,
    val uploadedAt: Long? = null,
)

data class FolderCounts(
    val folderId: Long,
    val uploaded: Int,
    val pending: Int,
    val failed: Int,
    val skipped: Int,
)

data class FileFailure(
    val folderId: Long,
    val name: String,
    val lastError: String,
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

    @Query(
        "UPDATE folders SET remoteFolderId = :remoteId, remotePath = :remotePath, " +
            "remoteOrigin = :origin, remoteAccount = :account WHERE id = :id",
    )
    suspend fun setRemote(
        id: Long,
        remoteId: String?,
        remotePath: String?,
        origin: String?,
        account: String?,
    )

    @Query("UPDATE folders SET remoteFolderId = NULL, remotePath = NULL WHERE id = :id")
    suspend fun clearRemote(id: Long)

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

    @Query(
        "SELECT * FROM files WHERE folderId = :folderId AND uri = :uri LIMIT 1",
    )
    suspend fun byUri(folderId: Long, uri: String): FileEntity?

    @Query("SELECT COUNT(*) FROM files WHERE folderId = :folderId AND status = :status")
    suspend fun countStatus(folderId: Long, status: FileStatus): Int

    @Insert(onConflict = OnConflictStrategy.IGNORE)
    suspend fun insert(file: FileEntity): Long

    @Query("SELECT * FROM files WHERE folderId = :folderId AND parentPath = :parentPath AND name = :name LIMIT 1")
    suspend fun byRelativePath(folderId: Long, parentPath: String, name: String): FileEntity?

    @Query(
        "UPDATE files SET status = :status, sha256 = :sha, remoteNodeId = :remoteNodeId, " +
            "remoteName = :remoteName, contentDirty = 0, moveDirty = 0, uploadedAt = :uploadedAt, " +
            "attempts = 0, lastError = NULL WHERE id = :id",
    )
    suspend fun markUploaded(
        id: Long,
        sha: String?,
        remoteNodeId: String?,
        remoteName: String?,
        uploadedAt: Long,
        status: FileStatus,
    )

    @Query(
        "UPDATE files SET sha256 = :sha, remoteNodeId = :remoteNodeId, remoteName = :remoteName, " +
            "contentDirty = 0, status = :status, attempts = attempts + 1, lastError = :error " +
            "WHERE id = :id",
    )
    suspend fun markContentSynced(
        id: Long,
        sha: String,
        remoteNodeId: String,
        remoteName: String?,
        status: FileStatus,
        error: String,
    )

    @Query(
        "UPDATE files SET status = :status, attempts = attempts + 1, lastError = :error WHERE id = :id",
    )
    suspend fun markFailed(id: Long, status: FileStatus, error: String)

    @Query(
        "UPDATE files SET size = :size, mtime = :mtime, name = :name, parentPath = :parentPath, " +
            "status = :status, sha256 = :sha, contentDirty = :contentDirty, moveDirty = :moveDirty, " +
            "attempts = :attempts, lastError = NULL, " +
            "uploadedAt = CASE WHEN :contentDirty = 1 THEN NULL ELSE uploadedAt END WHERE id = :id",
    )
    suspend fun markChanged(
        id: Long,
        size: Long,
        mtime: Long,
        name: String,
        parentPath: String,
        status: FileStatus,
        sha: String?,
        contentDirty: Boolean,
        moveDirty: Boolean,
        attempts: Int,
    )

    @Query(
        "UPDATE files SET remoteNodeId = NULL, remoteName = NULL, sha256 = NULL, " +
            "contentDirty = 1, moveDirty = 0, status = 'PENDING', attempts = 0, " +
            "lastError = NULL, uploadedAt = NULL WHERE folderId = :folderId",
    )
    suspend fun clearRemoteIdsForFolder(folderId: Long)

    @Query(
        "UPDATE files SET remoteNodeId = NULL, remoteName = NULL, contentDirty = 1, " +
            "moveDirty = 0, status = 'PENDING', attempts = 0, lastError = NULL WHERE id = :id",
    )
    suspend fun clearRemoteId(id: Long)

    @Query(
        "UPDATE files SET status = 'PENDING', attempts = 0, lastError = NULL " +
            "WHERE folderId = :folderId AND status IN ('FAILED', 'SKIPPED')",
    )
    suspend fun resetFailures(folderId: Long)

    @Query("DELETE FROM files WHERE id = :id")
    suspend fun delete(id: Long)

    @Query("DELETE FROM files WHERE folderId = :folderId")
    suspend fun deleteAllForFolder(folderId: Long)

    @Query(
        "SELECT folderId, " +
            "SUM(CASE WHEN status = 'UPLOADED' THEN 1 ELSE 0 END) AS uploaded, " +
            "SUM(CASE WHEN status = 'PENDING' THEN 1 ELSE 0 END) AS pending, " +
            "SUM(CASE WHEN status = 'FAILED' THEN 1 ELSE 0 END) AS failed, " +
            "SUM(CASE WHEN status = 'SKIPPED' THEN 1 ELSE 0 END) AS skipped " +
            "FROM files GROUP BY folderId",
    )
    fun observeCounts(): Flow<List<FolderCounts>>

    @Query("SELECT COALESCE(SUM(size), 0) FROM files WHERE status = 'UPLOADED'")
    fun observeTotalUploaded(): Flow<Long>

    @Query(
        "SELECT folderId, name, lastError FROM files " +
            "WHERE status IN ('FAILED', 'SKIPPED') AND lastError IS NOT NULL " +
            "ORDER BY folderId, name",
    )
    fun observeFailures(): Flow<List<FileFailure>>
}

@Database(
    entities = [FolderEntity::class, FileEntity::class],
    version = 2,
    exportSchema = true,
)
abstract class AppDb : RoomDatabase() {
    abstract fun folderDao(): FolderDao
    abstract fun fileDao(): FileDao

    /** Removes the folder and every tracked file atomically (FK ON DELETE CASCADE). */
    @Transaction
    open suspend fun removeFolder(folderId: Long) {
        folderDao().delete(folderId)
    }

    companion object {
        @Volatile private var instance: AppDb? = null

        val MIGRATION_1_2 = object : Migration(1, 2) {
            override fun migrate(db: SupportSQLiteDatabase) {
                db.execSQL("ALTER TABLE folders ADD COLUMN remoteOrigin TEXT")
                db.execSQL("ALTER TABLE folders ADD COLUMN remoteAccount TEXT")
                db.execSQL(
                    "CREATE TABLE IF NOT EXISTS `files_new` (" +
                        "`id` INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL, " +
                        "`folderId` INTEGER NOT NULL, " +
                        "`uri` TEXT NOT NULL, " +
                        "`parentPath` TEXT NOT NULL, " +
                        "`name` TEXT NOT NULL, " +
                        "`size` INTEGER NOT NULL, " +
                        "`mtime` INTEGER NOT NULL, " +
                        "`sha256` TEXT, " +
                        "`status` TEXT NOT NULL, " +
                        "`remoteNodeId` TEXT, " +
                        "`remoteName` TEXT, " +
                        "`contentDirty` INTEGER NOT NULL, " +
                        "`moveDirty` INTEGER NOT NULL, " +
                        "`attempts` INTEGER NOT NULL, " +
                        "`lastError` TEXT, " +
                        "`uploadedAt` INTEGER, " +
                        "FOREIGN KEY(`folderId`) REFERENCES `folders`(`id`) " +
                        "ON UPDATE NO ACTION ON DELETE CASCADE)",
                )
                db.execSQL(
                    "INSERT INTO `files_new` (" +
                        "`id`, `folderId`, `uri`, `parentPath`, `name`, `size`, `mtime`, `sha256`, " +
                        "`status`, `remoteNodeId`, `remoteName`, `contentDirty`, `moveDirty`, " +
                        "`attempts`, `lastError`, `uploadedAt`) " +
                        "SELECT `id`, `folderId`, `uri`, `parentPath`, `name`, `size`, `mtime`, " +
                        "`sha256`, `status`, `remoteNodeId`, NULL, " +
                        "CASE WHEN `status` = 'UPLOADED' THEN 0 ELSE 1 END, 0, " +
                        "`attempts`, `lastError`, `uploadedAt` FROM `files`",
                )
                db.execSQL("DROP TABLE `files`")
                db.execSQL("ALTER TABLE `files_new` RENAME TO `files`")
                db.execSQL(
                    "CREATE UNIQUE INDEX IF NOT EXISTS `index_files_folderId_uri` " +
                        "ON `files` (`folderId`, `uri`)",
                )
                db.execSQL(
                    "CREATE INDEX IF NOT EXISTS `index_files_folderId` ON `files` (`folderId`)",
                )
            }
        }

        fun get(context: Context): AppDb = instance ?: synchronized(this) {
            instance ?: build(context).also { instance = it }
        }

        private fun build(context: Context): AppDb =
            Room.databaseBuilder(context, AppDb::class.java, "nimbo-backup.db")
                .addMigrations(MIGRATION_1_2)
                .build()
    }
}
