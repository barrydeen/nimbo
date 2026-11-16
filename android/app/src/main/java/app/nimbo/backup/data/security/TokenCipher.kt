package app.nimbo.backup.data.security

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.security.KeyStore
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey

/**
 * Android Keystore-backed encryption for the long-lived device token.
 *
 * Only the Keystore-wrapped key can decrypt the DataStore value; a copied app
 * data directory is not enough to recover the token. The key is non-exportable
 * and tied to this app installation.
 */
object TokenCipher {

    private const val KEYSTORE = "AndroidKeyStore"
    private const val ALIAS = "nimbo.device-token.v1"
    private const val TRANSFORMATION = "AES/GCM/NoPadding"

    /** Encrypts a token, returning null when secure storage is unavailable. */
    fun encrypt(plaintext: String): String? = try {
        AesGcm.encrypt(key(), plaintext)
    } catch (_: Exception) {
        null
    }

    /** Decrypts a Keystore-backed envelope, or null when it cannot be read. */
    fun decrypt(envelope: String): String? = try {
        AesGcm.decrypt(key(), envelope)
    } catch (_: Exception) {
        null
    }

    /**
     * Reads a legacy plaintext value (pre-migration) or a Keystore envelope.
     * Returns null only when an encrypted value cannot be decrypted.
     */
    fun open(stored: String): String? = when {
        stored.isBlank() -> ""
        AesGcm.isEncrypted(stored) -> decrypt(stored)
        else -> stored
    }

    private fun key(): SecretKey {
        val store = KeyStore.getInstance(KEYSTORE).apply { load(null) }
        val existing = store.getEntry(ALIAS, null) as? KeyStore.SecretKeyEntry
        if (existing != null) return existing.secretKey

        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
        generator.init(
            KeyGenParameterSpec.Builder(
                ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .setUserAuthenticationRequired(false)
                .build(),
        )
        return generator.generateKey()
    }
}
