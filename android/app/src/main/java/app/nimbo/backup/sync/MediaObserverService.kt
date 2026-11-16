package app.nimbo.backup.sync

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.database.ContentObserver
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.provider.MediaStore
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import app.nimbo.backup.MainActivity
import app.nimbo.backup.R
import app.nimbo.backup.data.Prefs
import app.nimbo.backup.data.isConfigured
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.firstOrNull
import kotlinx.coroutines.launch

/**
 * Real-time fast path for camera photos, screenshots and downloads.
 *
 * A lightweight foreground service watches MediaStore and fires a one-shot backup
 * (Wi-Fi-only) shortly after content changes settle. The 15-minute periodic worker
 * remains the reliable baseline for everything MediaStore doesn't cover.
 */
class MediaObserverService : Service() {

    private val handler = Handler(Looper.getMainLooper())
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var observers: List<ContentObserver> = emptyList()

    private val trigger = Runnable { BackupWorker.requestSync(this) }

    private val debounce: ContentObserver.() -> Unit = {
        handler.removeCallbacks(trigger)
        handler.postDelayed(trigger, DEBOUNCE_MS)
    }

    override fun onCreate() {
        super.onCreate()
        startForeground(NOTIFICATION_ID, buildNotification())
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

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int = START_STICKY

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        observers.forEach { contentResolver.unregisterContentObserver(it) }
        observers = emptyList()
        handler.removeCallbacks(trigger)
        super.onDestroy()
    }

    private fun buildNotification(): Notification {
        val nm = getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            nm.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_ID,
                    getString(R.string.notif_channel_name),
                    NotificationManager.IMPORTANCE_LOW,
                ),
            )
        }
        val openApp = PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_stat_upload)
            .setContentTitle(getString(R.string.notif_title))
            .setContentText(getString(R.string.notif_text))
            .setOngoing(true)
            .setSilent(true)
            .setContentIntent(openApp)
            .build()
    }

    companion object {
        private const val CHANNEL_ID = "nimbo_backup"
        private const val NOTIFICATION_ID = 42
        private const val DEBOUNCE_MS = 5_000L

        /** Idempotent start; no-ops when instant mode is off. */
        fun maybeStart(context: Context) {
            val prefs = Prefs(context)
            scopeSafe(context) {
                if (prefs.instantMode.first() && prefs.config.firstOrNull()?.isConfigured == true) {
                    ContextCompat.startForegroundService(context, Intent(context, MediaObserverService::class.java))
                }
            }
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, MediaObserverService::class.java))
        }

        private fun scopeSafe(context: Context, block: suspend () -> Unit) {
            CoroutineScope(SupervisorJob() + Dispatchers.IO).launch {
                try {
                    block()
                } catch (_: Exception) {
                }
            }
        }
    }
}
