//! Secret variables: ChaCha20-Poly1305 with a key stored at `<home>/secret.key`
//! (created with owner-only permissions on first use).

use std::path::Path;

use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;

pub const KEY_FILE: &str = "secret.key";

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error(
        "secret key {0} is missing; secrets encrypted with it are unrecoverable without the key"
    )]
    MissingKey(String),
    #[error("secret key is invalid")]
    BadKey,
    #[error("ciphertext is invalid or was encrypted with a different key")]
    BadCiphertext,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn key_path(home: &Path) -> std::path::PathBuf {
    home.join(KEY_FILE)
}

pub fn key_exists(home: &Path) -> bool {
    key_path(home).exists()
}

/// Load the key, creating it on first use.
pub fn load_or_create_key(home: &Path) -> Result<[u8; 32], SecretError> {
    let path = key_path(home);
    if path.exists() {
        return load_key(home);
    }
    let mut key = [0u8; 32];
    rand::rng().fill_bytes(&mut key);
    let encoded = base64::engine::general_purpose::STANDARD.encode(key);
    std::fs::create_dir_all(home)?;
    std::fs::write(&path, encoded)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(key)
}

pub fn load_key(home: &Path) -> Result<[u8; 32], SecretError> {
    let path = key_path(home);
    let text = std::fs::read_to_string(&path)
        .map_err(|_| SecretError::MissingKey(path.display().to_string()))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_err(|_| SecretError::BadKey)?;
    let key: [u8; 32] = bytes.try_into().map_err(|_| SecretError::BadKey)?;
    Ok(key)
}

/// Encrypt plain text; the result is `v1:<base64 nonce||ciphertext>`.
pub fn encrypt(home: &Path, plain: &str) -> Result<String, SecretError> {
    let key = load_or_create_key(home)?;
    let cipher = ChaCha20Poly1305::new((&key).into());
    let mut nonce_bytes = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let sealed = cipher
        .encrypt(nonce, plain.as_bytes())
        .map_err(|_| SecretError::BadKey)?;
    let mut blob = nonce_bytes.to_vec();
    blob.extend(sealed);
    Ok(format!(
        "v1:{}",
        base64::engine::general_purpose::STANDARD.encode(blob)
    ))
}

pub fn decrypt(home: &Path, text: &str) -> Result<String, SecretError> {
    let key = load_key(home)?;
    let encoded = text.strip_prefix("v1:").ok_or(SecretError::BadCiphertext)?;
    let blob = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| SecretError::BadCiphertext)?;
    if blob.len() < 12 {
        return Err(SecretError::BadCiphertext);
    }
    let (nonce_bytes, sealed) = blob.split_at(12);
    let cipher = ChaCha20Poly1305::new((&key).into());
    let plain = cipher
        .decrypt(Nonce::from_slice(nonce_bytes), sealed)
        .map_err(|_| SecretError::BadCiphertext)?;
    String::from_utf8(plain).map_err(|_| SecretError::BadCiphertext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_missing_key() {
        let dir = tempfile::TempDir::new().unwrap();
        let sealed = encrypt(dir.path(), "hunter2").unwrap();
        assert!(sealed.starts_with("v1:"));
        assert_eq!(decrypt(dir.path(), &sealed).unwrap(), "hunter2");
        std::fs::remove_file(dir.path().join(KEY_FILE)).unwrap();
        let err = decrypt(dir.path(), &sealed).unwrap_err();
        assert!(err.to_string().contains("unrecoverable"));
    }
}
