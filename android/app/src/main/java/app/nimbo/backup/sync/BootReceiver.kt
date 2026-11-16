package app.nimbo.backup.sync

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Restarts the real-time observer after reboot or app update when instant mode is on. */
class BootReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        val action = intent.action ?: return
        if (action == Intent.ACTION_BOOT_COMPLETED || action == Intent.ACTION_MY_PACKAGE_REPLACED) {
            MediaObserverService.maybeStart(context)
        }
    }
}
