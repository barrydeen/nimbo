use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use rand::RngCore;
use std::{fs, path::Path};

/// Loads the instance secret from disk, generating it on first boot.
pub fn load_or_create_secret(data_dir: &Path) -> Result<[u8; 32]> {
    let path = data_dir.join("secret.key");
    if path.exists() {
        let bytes = fs::read(&path).context("reading secret.key")?;
        if bytes.len() != 32 {
            anyhow::bail!("secret.key must contain exactly 32 bytes");
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        Ok(key)
    } else {
        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        fs::write(&path, key).context("writing secret.key")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(key)
    }
}

/// Encrypts a settings value at rest. Output is base64(nonce || ciphertext).
#[allow(dead_code)]
pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<String> {
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| anyhow::anyhow!("invalid key length"))?;
    let mut nonce_bytes = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| anyhow::anyhow!("encryption failed"))?;
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&ciphertext);
    Ok(STANDARD.encode(out))
}

#[allow(dead_code)]
pub fn decrypt(key: &[u8; 32], encoded: &str) -> Result<Vec<u8>> {
    let raw = STANDARD.decode(encoded).context("base64 decode")?;
    if raw.len() < 24 {
        anyhow::bail!("ciphertext too short");
    }
    let (nonce_bytes, ciphertext) = raw.split_at(24);
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| anyhow::anyhow!("invalid key length"))?;
    cipher
        .decrypt(XNonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|_| anyhow::anyhow!("decryption failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let key = [7u8; 32];
        let enc = encrypt(&key, b"hello world").unwrap();
        assert_eq!(decrypt(&key, &enc).unwrap(), b"hello world");
    }

    #[test]
    fn wrong_key_fails() {
        let enc = encrypt(&[1u8; 32], b"secret").unwrap();
        assert!(decrypt(&[2u8; 32], &enc).is_err());
    }
}
