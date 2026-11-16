package app.nimbo.backup.sync

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import app.nimbo.backup.util.SafeLog
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch

/**
 * After reboot or an app update, restore the reliable WorkManager baseline.
 *
 * Android 15+ forbids starting a `dataSync` foreground service from
 * BOOT_COMPLETED, so the receiver never launches the observer. It only enqueues
 * constrained WorkManager work; the user re-enables instant mode from the app.
 */
class BootReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        val action = intent.action ?: return
        if (action != Intent.ACTION_BOOT_COMPLETED && action != Intent.ACTION_MY_PACKAGE_REPLACED) {
            return
        }
        val pending = goAsync()
        CoroutineScope(SupervisorJob() + Dispatchers.IO).launch {
            try {
                BackupWorker.schedulePeriodic(context.applicationContext)
                BackupWorker.requestSync(context.applicationContext)
            } catch (e: Exception) {
                SafeLog.w(TAG, "boot scheduling failed", e)
            } finally {
                pending.finish()
            }
        }
    }

    companion object {
        private const val TAG = "NimboBackup"
    }
}
