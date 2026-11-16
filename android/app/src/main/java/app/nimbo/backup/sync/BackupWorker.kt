package app.nimbo.backup.sync

import android.content.Context
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.util.concurrent.TimeUnit

/**
 * Scans every enabled folder and uploads pending files. Wi-Fi-only: every request
 * carries an UNMETERED network constraint.
 */
class BackupWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {

    override suspend fun doWork(): Result = withContext(Dispatchers.IO) {
        when (val outcome = BackupEngine.run(applicationContext)) {
            is BackupEngine.Outcome.NotConfigured -> {
                WorkManager.getInstance(applicationContext).cancelUniqueWork(PERIODIC)
                Result.success()
            }
            is BackupEngine.Outcome.Ok -> {
                // Transient failures (server down, network flake) retry on backoff;
                // engine state in Room guarantees idempotence, so retries are cheap.
                if (outcome.errors.isEmpty()) Result.success() else Result.retry()
            }
        }
    }

    companion object {
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
                    .setBackoffCriteria(androidx.work.BackoffPolicy.EXPONENTIAL, 5, TimeUnit.MINUTES)
                    .build(),
            )
        }

        /** One-shot sync (backup-now button, media observer, boot). */
        fun requestSync(context: Context) {
            WorkManager.getInstance(context).enqueueUniqueWork(
                ONESHOT,
                ExistingWorkPolicy.REPLACE,
                OneTimeWorkRequestBuilder<BackupWorker>()
                    .setConstraints(constraints())
                    .setBackoffCriteria(androidx.work.BackoffPolicy.EXPONENTIAL, 5, TimeUnit.MINUTES)
                    .build(),
            )
        }
    }
}
