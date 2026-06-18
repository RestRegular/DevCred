//! Field-level encryption: AES-256-GCM with an Argon2id-derived key.
//!
//! The master password never touches disk. It is run through Argon2id with a
//! per-vault salt (stored in the `meta` table) to derive a 256-bit key, which
//! is then used to seal each secret value with AES-256-GCM. A fresh 96-bit
//! nonce is generated per encryption and prepended to the ciphertext.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{Result, anyhow};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;
use zeroize::Zeroize;

/// 96-bit GCM nonce length in bytes.
pub const NONCE_LEN: usize = 12;
/// 256-bit key length in bytes.
pub const KEY_LEN: usize = 32;
/// 128-bit salt length in bytes.
pub const SALT_LEN: usize = 16;

/// A derived 256-bit master key. Zeroized on drop.
#[derive(Clone, Zeroize)]
pub struct MasterKey([u8; KEY_LEN]);

impl MasterKey {
    /// Derive a key from the master password and salt using Argon2id.
    ///
    /// Parameters: 64 MiB memory, 3 iterations, 1 lane — a reasonable default
    /// for a local interactive tool.
    pub fn derive(password: &str, salt: &[u8]) -> Result<Self> {
        if salt.len() < SALT_LEN {
            return Err(anyhow!("salt too short (need {SALT_LEN} bytes)"));
        }
        let params = Params::new(64 * 1024, 3, 1, Some(KEY_LEN))
            .map_err(|e| anyhow!("argon2 params: {e}"))?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut key = [0u8; KEY_LEN];
        argon
            .hash_password_into(password.as_bytes(), salt, &mut key)
            .map_err(|e| anyhow!("argon2 derive failed: {e}"))?;
        Ok(MasterKey(key))
    }

    /// Encrypt a plaintext secret. Returns `nonce || ciphertext`.
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let cipher = Aes256Gcm::new(&self.0.into());
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| anyhow!("aes-gcm encrypt: {e}"))?;
        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Decrypt a blob produced by [`Self::seal`].
    pub fn open(&self, blob: &[u8]) -> Result<Vec<u8>> {
        if blob.len() < NONCE_LEN {
            return Err(anyhow!("ciphertext too short"));
        }
        let (nonce_bytes, ct) = blob.split_at(NONCE_LEN);
        let cipher = Aes256Gcm::new(&self.0.into());
        let nonce = Nonce::from_slice(nonce_bytes);
        cipher
            .decrypt(nonce, ct)
            .map_err(|_| anyhow!("wrong master password or corrupted data"))
    }
}

/// Generate a fresh random salt for a new vault.
pub fn new_salt() -> [u8; SALT_LEN] {
    let mut salt = [0u8; SALT_LEN];
    rand::thread_rng().fill_bytes(&mut salt);
    salt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let salt = new_salt();
        let key = MasterKey::derive("correct horse battery staple", &salt).unwrap();
        let blob = key.seal(b"ghp_supersecret").unwrap();
        let pt = key.open(&blob).unwrap();
        assert_eq!(pt, b"ghp_supersecret");
    }

    #[test]
    fn wrong_password_fails() {
        let salt = new_salt();
        let key = MasterKey::derive("hunter2", &salt).unwrap();
        let blob = key.seal(b"secret").unwrap();

        let wrong = MasterKey::derive("hunter3", &salt).unwrap();
        assert!(wrong.open(&blob).is_err());
    }

    #[test]
    fn nonce_is_unique() {
        let salt = new_salt();
        let key = MasterKey::derive("pw", &salt).unwrap();
        let a = key.seal(b"x").unwrap();
        let b = key.seal(b"x").unwrap();
        assert_ne!(a, b, "nonces must not repeat");
    }
}
