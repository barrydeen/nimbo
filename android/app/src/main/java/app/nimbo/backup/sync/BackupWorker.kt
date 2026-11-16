package app.nimbo.backup.sync

import android.content.Context
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import app.nimbo.backup.util.SafeLog
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import java.util.concurrent.TimeUnit

/**
 * Runs one backup pass under the engine's process-wide mutex.
 *
 * Periodic and one-shot work share the same unmetered-network constraints and
 * the same unique names/policies across triggers. If triggers arrive while a
 * pass is running, `KEEP` plus the engine's rerun flag coalesces them instead of
 * starting overlapping executors. Transient failures return `Result.retry()`
 * so WorkManager's exponential backoff actually applies; auth rejection pauses
 * scheduling until the user re-pairs.
 */
class BackupWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {

    override suspend fun doWork(): Result = withContext(Dispatchers.IO) {
        currentCoroutineContext().ensureActive()
        val outcome = try {
            BackupEngine.run(applicationContext)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            SafeLog.w(TAG, "backup worker failed", e)
            return@withContext Result.retry()
        }

        when (outcome) {
            is BackupEngine.Outcome.NotConfigured -> {
                cancelAll(applicationContext)
                Result.success()
            }
            is BackupEngine.Outcome.AuthRejected -> {
                // Do not hammer a revoked/expired credential. Re-pairing calls
                // Prefs.save(), which clears authFailed and schedules work again.
                pauseScheduling(applicationContext)
                Result.failure()
            }
            is BackupEngine.Outcome.ConfigurationError -> {
                app.nimbo.backup.data.Prefs(applicationContext)
                    .setObserverError("Backup needs attention: ${outcome.message}")
                pauseScheduling(applicationContext)
                Result.failure()
            }
            is BackupEngine.Outcome.Ok -> {
                if (outcome.transientFailures) Result.retry() else Result.success()
            }
        }
    }

    companion object {
        private const val TAG = "NimboBackup"
        const val PERIODIC = "nimbo-periodic-backup"
        const val ONESHOT = "nimbo-oneshot-backup"

        private fun constraints(): Constraints = Constraints.Builder()
            .setRequiredNetworkType(NetworkType.UNMETERED)
            .build()

        /** 15-minute safety-net scan for every folder type, Doze-compatible. */
        fun schedulePeriodic(context: Context) {
            WorkManager.getInstance(context).enqueueUniquePeriodicWork(
                PERIODIC,
                ExistingPeriodicWorkPolicy.KEEP,
                PeriodicWorkRequestBuilder<BackupWorker>(15, TimeUnit.MINUTES)
                    .setConstraints(constraints())
                    .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 15, TimeUnit.MINUTES)
                    .build(),
            )
        }

        /** Coalesced one-shot sync (backup-now, media observer, boot recovery). */
        fun requestSync(context: Context) {
            BackupEngine.requestRerun()
            WorkManager.getInstance(context).enqueueUniqueWork(
                ONESHOT,
                ExistingWorkPolicy.KEEP,
                OneTimeWorkRequestBuilder<BackupWorker>()
                    .setConstraints(constraints())
                    .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 5, TimeUnit.MINUTES)
                    .build(),
            )
        }

        /** Cancels both execution paths; running calls observe coroutine cancellation. */
        fun cancelAll(context: Context) {
            WorkManager.getInstance(context).apply {
                cancelUniqueWork(PERIODIC)
                cancelUniqueWork(ONESHOT)
            }
        }

        fun pauseScheduling(context: Context) = cancelAll(context)
    }
}
