package app.nimbo.backup.data

import android.content.Context
import android.os.Build
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import app.nimbo.backup.sync.BackupWorker
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map

private val Context.dataStore by preferencesDataStore(name = "nimbo")

data class ServerConfig(
    val serverUrl: String,
    val token: String,
    val deviceName: String,
)

/** True when [ServerConfig] is complete enough to talk to the server. */
val ServerConfig.isConfigured: Boolean
    get() = serverUrl.isNotBlank() && token.isNotBlank()

class Prefs(private val context: Context) {

    private object Keys {
        val SERVER_URL = stringPreferencesKey("server_url")
        val TOKEN = stringPreferencesKey("token")
        val DEVICE_NAME = stringPreferencesKey("device_name")
        val INSTANT_MODE = booleanPreferencesKey("instant_mode")
        val AUTH_FAILED = booleanPreferencesKey("auth_failed")
    }

    val config: Flow<ServerConfig?> = context.dataStore.data.map { p ->
        val url = p[Keys.SERVER_URL]?.trim()?.trimEnd('/') ?: ""
        val token = p[Keys.TOKEN] ?: ""
        if (url.isBlank() || token.isBlank()) return@map null
        ServerConfig(url, token, p[Keys.DEVICE_NAME] ?: Build.MODEL ?: "Phone")
    }.distinctUntilChanged()

    val instantMode: Flow<Boolean> = context.dataStore.data
        .map { it[Keys.INSTANT_MODE] ?: false }
        .distinctUntilChanged()

    /** Set when the server answered 401 with the stored token; cleared on re-pair. */
    val authFailed: Flow<Boolean> = context.dataStore.data
        .map { it[Keys.AUTH_FAILED] ?: false }
        .distinctUntilChanged()

    suspend fun save(config: ServerConfig) {
        context.dataStore.edit { p ->
            p[Keys.SERVER_URL] = config.serverUrl.trim().trimEnd('/')
            p[Keys.TOKEN] = config.token.trim()
            p[Keys.DEVICE_NAME] = config.deviceName.ifBlank { Build.MODEL ?: "Phone" }
            p[Keys.AUTH_FAILED] = false
        }
        BackupWorker.schedulePeriodic(context)
    }

    suspend fun setInstantMode(enabled: Boolean) =
        context.dataStore.edit { it[Keys.INSTANT_MODE] = enabled }

    suspend fun markAuthFailed() =
        context.dataStore.edit { it[Keys.AUTH_FAILED] = true }
}
