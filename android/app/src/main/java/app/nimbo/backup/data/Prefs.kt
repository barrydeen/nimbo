package app.nimbo.backup.data

import android.content.Context
import android.os.Build
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import app.nimbo.backup.data.security.AesGcm
import app.nimbo.backup.data.security.TokenCipher
import app.nimbo.backup.sync.BackupWorker
import app.nimbo.backup.util.SafeLog
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.flowOn
import kotlinx.coroutines.flow.map
import java.io.IOException

private val Context.dataStore by preferencesDataStore(name = "nimbo")

data class ServerConfig(
    val serverUrl: String,
    val token: String,
    val deviceName: String,
    val allowInsecureHttp: Boolean = false,
)

/** True when [ServerConfig] is complete enough to talk to the server. */
val ServerConfig.isConfigured: Boolean
    get() = serverUrl.isNotBlank() && token.isNotBlank()

class Prefs(private val context: Context) {

    private val appContext = context.applicationContext

    private object Keys {
        val SERVER_URL = stringPreferencesKey("server_url")
        val TOKEN = stringPreferencesKey("token")
        val DEVICE_NAME = stringPreferencesKey("device_name")
        val INSTANT_MODE = booleanPreferencesKey("instant_mode")
        val AUTH_FAILED = booleanPreferencesKey("auth_failed")
        val ALLOW_INSECURE_HTTP = booleanPreferencesKey("allow_insecure_http")
        val OBSERVER_ERROR = stringPreferencesKey("observer_error")
    }

    val config: Flow<ServerConfig?> = appContext.dataStore.data.map { p ->
        val url = p[Keys.SERVER_URL]?.trim()?.trimEnd('/') ?: ""
        val storedToken = p[Keys.TOKEN].orEmpty()
        val token = when {
            storedToken.isBlank() -> ""
            AesGcm.isEncrypted(storedToken) -> TokenCipher.decrypt(storedToken).orEmpty()
            else -> storedToken // legacy plaintext until migrateLegacy() rewrites it
        }
        if (url.isBlank() || token.isBlank()) return@map null
        ServerConfig(
            serverUrl = url,
            token = token,
            deviceName = p[Keys.DEVICE_NAME] ?: Build.MODEL ?: "Phone",
            allowInsecureHttp = p[Keys.ALLOW_INSECURE_HTTP] ?: false,
        )
    }.flowOn(Dispatchers.IO).distinctUntilChanged()

    val instantMode: Flow<Boolean> = appContext.dataStore.data
        .map { it[Keys.INSTANT_MODE] ?: false }
        .distinctUntilChanged()

    /** Set when the server answered 401 with the stored token; cleared on re-pair. */
    val authFailed: Flow<Boolean> = appContext.dataStore.data
        .map { it[Keys.AUTH_FAILED] ?: false }
        .distinctUntilChanged()

    /** User-visible reason real-time observation is not currently available. */
    val observerError: Flow<String?> = appContext.dataStore.data
        .map { it[Keys.OBSERVER_ERROR] }
        .distinctUntilChanged()

    /**
     * Encrypts a legacy plaintext token from installations that predate the
     * Keystore-backed format. Safe to call repeatedly.
     */
    suspend fun migrateLegacy() {
        val snapshot = appContext.dataStore.data.first()
        val stored = snapshot[Keys.TOKEN].orEmpty()
        if (stored.isBlank() || AesGcm.isEncrypted(stored)) return
        val encrypted = TokenCipher.encrypt(stored)
        if (encrypted == null) {
            SafeLog.w(TAG, "legacy token migration failed")
            return
        }
        appContext.dataStore.edit { p -> p[Keys.TOKEN] = encrypted }
    }

    suspend fun save(config: ServerConfig) {
        val encrypted = TokenCipher.encrypt(config.token.trim())
            ?: throw IOException("secure token storage is unavailable on this device")
        appContext.dataStore.edit { p ->
            p[Keys.SERVER_URL] = config.serverUrl.trim().trimEnd('/')
            p[Keys.TOKEN] = encrypted
            p[Keys.DEVICE_NAME] = config.deviceName.ifBlank { Build.MODEL ?: "Phone" }
            p[Keys.ALLOW_INSECURE_HTTP] = config.allowInsecureHttp
            p[Keys.AUTH_FAILED] = false
            p[Keys.OBSERVER_ERROR] = ""
        }
        if (config.isConfigured) BackupWorker.schedulePeriodic(appContext)
    }

    /** Clears the pairing without deleting folder selections or backup history. */
    suspend fun clearPairing() {
        appContext.dataStore.edit { p ->
            p.remove(Keys.SERVER_URL)
            p.remove(Keys.TOKEN)
            p.remove(Keys.ALLOW_INSECURE_HTTP)
            p[Keys.AUTH_FAILED] = false
            p[Keys.OBSERVER_ERROR] = ""
        }
        BackupWorker.cancelAll(appContext)
    }

    suspend fun setInstantMode(enabled: Boolean) =
        appContext.dataStore.edit {
            it[Keys.INSTANT_MODE] = enabled
            if (!enabled) it[Keys.OBSERVER_ERROR] = ""
        }

    suspend fun markAuthFailed() =
        appContext.dataStore.edit { it[Keys.AUTH_FAILED] = true }

    suspend fun clearAuthFailed() =
        appContext.dataStore.edit { it[Keys.AUTH_FAILED] = false }

    suspend fun setObserverError(message: String?) =
        appContext.dataStore.edit { it[Keys.OBSERVER_ERROR] = message.orEmpty() }

    companion object {
        private const val TAG = "NimboBackup"
    }
}
