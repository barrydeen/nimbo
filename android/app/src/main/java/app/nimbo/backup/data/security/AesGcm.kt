package app.nimbo.backup.data.security

import java.security.SecureRandom
import java.util.Base64
import javax.crypto.Cipher
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Small, testable AES-256-GCM envelope helper.
 *
 * The production key lives in the Android Keystore; this class only performs
 * the authenticated encryption so the format can be exercised by JVM tests.
 * Stored format: `v1:<base64url-iv>:<base64url-ciphertext>`.
 */
object AesGcm {

    private const val VERSION = "v1"
    private const val IV_BYTES = 12
    private const val TAG_BITS = 128
    private val encoder: Base64.Encoder = Base64.getUrlEncoder().withoutPadding()
    private val decoder: Base64.Decoder = Base64.getUrlDecoder()
    private val random = SecureRandom()

    fun isEncrypted(value: String?): Boolean =
        value != null && value.startsWith("$VERSION:")

    fun encrypt(key: SecretKey, plaintext: String): String {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        // Never pass a caller nonce: AndroidKeyStore keys forbid it
        // (CALLER_NONCE_PROHIBITED) — the cipher generates a fresh IV and
        // reports it via cipher.iv. Plain JVM keys behave the same way.
        cipher.init(Cipher.ENCRYPT_MODE, key)
        val iv = cipher.iv
        val ciphertext = cipher.doFinal(plaintext.toByteArray(Charsets.UTF_8))
        return "$VERSION:${encoder.encodeToString(iv)}:${encoder.encodeToString(ciphertext)}"
    }

    /** Returns null for malformed, truncated or tampered envelopes. */
    fun decrypt(key: SecretKey, envelope: String): String? {
        if (!isEncrypted(envelope)) return null
        return try {
            val parts = envelope.split(':')
            if (parts.size != 3) return null
            val iv = decoder.decode(parts[1])
            if (iv.size != IV_BYTES) return null
            val ciphertext = decoder.decode(parts[2])
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(TAG_BITS, iv))
            String(cipher.doFinal(ciphertext), Charsets.UTF_8)
        } catch (_: Exception) {
            null
        }
    }
}
