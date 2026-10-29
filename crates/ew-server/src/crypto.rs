//! Encryption at rest.
//!
//! * A 256-bit **master key** lives in `data/master.key`, sealed with Windows
//!   DPAPI (machine scope) so copying the file to another machine is useless.
//! * Every exam gets its own random **exam key**, stored in the database only
//!   wrapped (AES-256-GCM) by the master key. Purging an exam deletes the
//!   wrapped key, which makes any leftover segment bytes unreadable.
//! * Each recorded frame is sealed individually so a segment that was cut
//!   short by a crash is still readable up to its last complete frame.

use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, Context, Result};
use rand::RngCore;

pub type KeyBytes = [u8; 32];

pub fn random_key() -> KeyBytes {
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    k
}

pub fn load_or_create_master_key(path: &Path) -> Result<KeyBytes> {
    if path.exists() {
        let sealed = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let raw = unseal(&sealed)?;
        return raw.as_slice().try_into().map_err(|_| anyhow!("master key has wrong length"));
    }
    let key = random_key();
    let sealed = seal(&key)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, sealed)?;
    Ok(key)
}

/// Wraps an exam key for storage: `nonce(12) || ciphertext+tag`.
pub fn wrap_key(master: &KeyBytes, key: &KeyBytes) -> Vec<u8> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(master));
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let mut out = nonce.to_vec();
    out.extend(cipher.encrypt(Nonce::from_slice(&nonce), key.as_slice()).expect("aes-gcm encrypt"));
    out
}

pub fn unwrap_key(master: &KeyBytes, wrapped: &[u8]) -> Result<KeyBytes> {
    anyhow::ensure!(wrapped.len() > 12, "wrapped key too short");
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(master));
    let raw = cipher
        .decrypt(Nonce::from_slice(&wrapped[..12]), &wrapped[12..])
        .map_err(|_| anyhow!("exam key unwrap failed (wrong master key?)"))?;
    raw.as_slice().try_into().map_err(|_| anyhow!("exam key has wrong length"))
}

/// Per-segment frame sealer. Nonce = 4-byte random segment prefix || 8-byte counter,
/// so a nonce is never reused under one exam key.
pub struct FrameSealer {
    cipher: Aes256Gcm,
    prefix: [u8; 4],
    counter: u64,
}

impl FrameSealer {
    pub fn new(key: &KeyBytes) -> Self {
        let mut prefix = [0u8; 4];
        rand::thread_rng().fill_bytes(&mut prefix);
        Self { cipher: Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key)), prefix, counter: 0 }
    }

    pub fn prefix(&self) -> [u8; 4] {
        self.prefix
    }

    pub fn seal(&mut self, plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
        let nonce = nonce(self.prefix, self.counter);
        self.counter += 1;
        self.cipher
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad })
            .expect("aes-gcm encrypt")
    }
}

pub struct FrameOpener {
    cipher: Aes256Gcm,
    prefix: [u8; 4],
    counter: u64,
}

impl FrameOpener {
    pub fn new(key: &KeyBytes, prefix: [u8; 4]) -> Self {
        Self { cipher: Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key)), prefix, counter: 0 }
    }

    pub fn open(&mut self, sealed: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        let nonce = nonce(self.prefix, self.counter);
        self.counter += 1;
        self.cipher
            .decrypt(Nonce::from_slice(&nonce), Payload { msg: sealed, aad })
            .map_err(|_| anyhow!("segment frame failed authentication"))
    }
}

fn nonce(prefix: [u8; 4], counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[..4].copy_from_slice(&prefix);
    n[4..].copy_from_slice(&counter.to_le_bytes());
    n
}

/// Seals small secrets (keys) to this machine.
#[cfg(windows)]
pub fn seal(data: &[u8]) -> Result<Vec<u8>> {
    dpapi::protect(data)
}
#[cfg(windows)]
pub fn unseal(data: &[u8]) -> Result<Vec<u8>> {
    dpapi::unprotect(data)
}

// Non-Windows builds are for development only; the key is stored as-is.
#[cfg(not(windows))]
pub fn seal(data: &[u8]) -> Result<Vec<u8>> {
    Ok(data.to_vec())
}
#[cfg(not(windows))]
pub fn unseal(data: &[u8]) -> Result<Vec<u8>> {
    Ok(data.to_vec())
}

#[cfg(windows)]
mod dpapi {
    use anyhow::{anyhow, Result};
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_LOCAL_MACHINE, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
    }

    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(out.pbData as *mut _));
        v
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>> {
        let input = blob(data);
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_LOCAL_MACHINE | CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .map_err(|e| anyhow!("DPAPI protect failed: {e}"))?;
            Ok(take(out))
        }
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
        let input = blob(data);
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
                .map_err(|e| anyhow!("DPAPI unprotect failed: {e}"))?;
            Ok(take(out))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_roundtrip_and_wrong_master() {
        let m = random_key();
        let k = random_key();
        let w = wrap_key(&m, &k);
        assert_eq!(unwrap_key(&m, &w).unwrap(), k);
        assert!(unwrap_key(&random_key(), &w).is_err());
    }

    #[test]
    fn frames_roundtrip_in_order_and_detect_tamper() {
        let k = random_key();
        let mut s = FrameSealer::new(&k);
        let a = s.seal(b"frame-a", b"seg");
        let b = s.seal(b"frame-b", b"seg");
        let mut o = FrameOpener::new(&k, s.prefix());
        assert_eq!(o.open(&a, b"seg").unwrap(), b"frame-a");
        let mut bad = b.clone();
        bad[0] ^= 1;
        assert!(o.open(&bad, b"seg").is_err());
    }

    #[test]
    fn master_key_persists() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("master.key");
        let a = load_or_create_master_key(&p).unwrap();
        let b = load_or_create_master_key(&p).unwrap();
        assert_eq!(a, b);
        // on Windows the file is DPAPI-sealed, never the raw key
        #[cfg(windows)]
        assert_ne!(std::fs::read(&p).unwrap(), a.to_vec());
    }
}
