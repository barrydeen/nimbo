package app.nimbo.backup.sync

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.database.ContentObserver
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.provider.MediaStore
import androidx.annotation.RequiresApi
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import app.nimbo.backup.MainActivity
import app.nimbo.backup.R
import app.nimbo.backup.data.Prefs
import app.nimbo.backup.data.isConfigured
import app.nimbo.backup.util.SafeLog
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.firstOrNull
import kotlinx.coroutines.launch

/**
 * Best-effort, user-initiated real-time fast path for camera photos,
 * screenshots and downloads.
 *
 * Android 15+ prohibits starting a `dataSync` foreground service from
 * BOOT_COMPLETED and limits these services to a few hours per day, so this is
 * deliberately not the reliability baseline: the 15-minute WorkManager scan
 * remains authoritative. On timeout the service stops itself instead of
 * crashing or being restarted.
 */
class MediaObserverService : Service() {

    private val handler = Handler(Looper.getMainLooper())
    private var observers: List<ContentObserver> = emptyList()

    private val trigger = Runnable { BackupWorker.requestSync(this) }

    private val debounce: ContentObserver.() -> Unit = {
        handler.removeCallbacks(trigger)
        handler.postDelayed(trigger, DEBOUNCE_MS)
    }

    override fun onCreate() {
        super.onCreate()
        if (!startAsForeground()) {
            stopSelf()
            return
        }
        reportError(null)
        val uris = listOf(
            MediaStore.Images.Media.EXTERNAL_CONTENT_URI,
            MediaStore.Video.Media.EXTERNAL_CONTENT_URI,
            MediaStore.Files.getContentUri("external"),
        )
        observers = uris.map { uri ->
            val observer = object : ContentObserver(handler) {
                override fun onChange(selfChange: Boolean) = debounce()
                override fun onChange(selfChange: Boolean, uri: Uri?) = debounce()
            }
            contentResolver.registerContentObserver(uri, true, observer)
            observer
        }
    }

    private fun startAsForeground(): Boolean = try {
        val notification = buildNotification()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            ServiceCompat.startForeground(
                this,
                NOTIFICATION_ID,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
            )
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
        true
    } catch (e: Exception) {
        SafeLog.e(TAG, "real-time service could not start", e)
        reportError(
            "Real-time backup could not start on this device. " +
                "Periodic backups continue to run on unmetered networks.",
        )
        false
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int = START_NOT_STICKY

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        observers.forEach { contentResolver.unregisterContentObserver(it) }
        observers = emptyList()
        handler.removeCallbacks(trigger)
        stopForeground(STOP_FOREGROUND_REMOVE)
        super.onDestroy()
    }

    /** Pre-35 timeout callback. */
    @Suppress("DEPRECATION")
    override fun onTimeout(startId: Int) {
        handleTimeout()
    }

    /** Android 15+ `dataSync` quota reached: stop promptly instead of crashing. */
    @RequiresApi(Build.VERSION_CODES.VANILLA_ICE_CREAM)
    override fun onTimeout(startId: Int, fgsType: Int) {
        handleTimeout()
    }

    private fun handleTimeout() {
        SafeLog.w(TAG, "real-time dataSync service reached its platform time limit")
        reportError(
            "Android stopped real-time backup after its time limit. " +
                "Periodic backups continue automatically; reopen the app to re-enable instant mode.",
        )
        stopSelf()
    }

    private fun reportError(message: String?) {
        val app = applicationContext
        CoroutineScope(SupervisorJob() + Dispatchers.IO).launch {
            Prefs(app).setObserverError(message)
        }
    }

    private fun buildNotification(): Notification {
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                getString(R.string.notif_channel_name),
                NotificationManager.IMPORTANCE_LOW,
            ).apply { setShowBadge(false) },
        )
        val openApp = PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_stat_upload)
            .setContentTitle(getString(R.string.notif_title))
            .setContentText(getString(R.string.notif_text))
            .setOnlyAlertOnce(true)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .setOngoing(true)
            .setSilent(true)
            .setContentIntent(openApp)
            .build()
    }

    companion object {
        private const val TAG = "NimboBackup"
        private const val CHANNEL_ID = "nimbo_backup"
        private const val NOTIFICATION_ID = 42
        private const val DEBOUNCE_MS = 5_000L

        /** Idempotent start; never called from BOOT_COMPLETED. */
        suspend fun maybeStart(context: Context) {
            val prefs = Prefs(context)
            val enabled = prefs.instantMode.first()
            val configured = prefs.config.firstOrNull()?.isConfigured == true
            val authFailed = prefs.authFailed.first()
            if (!enabled || !configured || authFailed) {
                stop(context)
                return
            }
            try {
                ContextCompat.startForegroundService(
                    context,
                    Intent(context, MediaObserverService::class.java),
                )
            } catch (e: Exception) {
                SafeLog.e(TAG, "foreground service start was rejected", e)
                prefs.setObserverError(
                    "Real-time backup could not start on this device. " +
                        "Periodic backups continue on unmetered networks.",
                )
            }
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, MediaObserverService::class.java))
        }
    }
}
