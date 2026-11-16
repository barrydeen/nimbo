package app.nimbo.backup.data.security

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import javax.crypto.KeyGenerator

class AesGcmTest {

    private fun key() = KeyGenerator.getInstance("AES").apply { init(256) }.generateKey()

    @Test
    fun `round trips and reports encrypted envelopes`() {
        val key = key()
        val envelope = AesGcm.encrypt(key, "nmb_secret_token")
        assertTrue(AesGcm.isEncrypted(envelope))
        assertFalse(AesGcm.isEncrypted("legacy-plaintext"))
        assertEquals("nmb_secret_token", AesGcm.decrypt(key, envelope))
    }

    @Test
    fun `wrong key and tampering fail closed`() {
        val key = key()
        val envelope = AesGcm.encrypt(key, "nmb_secret_token")
        assertNull(AesGcm.decrypt(key(), envelope))

        val parts = envelope.split(':')
        val tampered = "${parts[0]}:${parts[1]}:${parts[2].dropLast(2)}AA"
        assertNull(AesGcm.decrypt(key, tampered))
        assertNull(AesGcm.decrypt(key, "v1:not-base64:not-base64"))
        assertNull(AesGcm.decrypt(key, "plaintext"))
    }

    @Test
    fun `each encryption uses a fresh iv`() {
        val key = key()
        assertTrue(
            AesGcm.encrypt(key, "same") != AesGcm.encrypt(key, "same"),
        )
    }

    @Test
    fun `legacy plaintext envelope is not decrypted as ciphertext`() {
        val key = key()
        assertNull(AesGcm.decrypt(key, "legacy-token"))
        assertFalse(AesGcm.isEncrypted("legacy-token"))
    }
}
