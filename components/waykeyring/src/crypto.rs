//! Key wrapping and sealing.
//!
//! * The master key (32 random bytes) encrypts every collection with
//!   XChaCha20-Poly1305. It is stored only wrapped: XChaCha20-Poly1305 under
//!   Argon2id(login password, salt).
//! * Attribute index: SHA-256(collection salt ‖ name ‖ 0 ‖ value), so a
//!   locked collection can still answer searches without revealing values.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

pub const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;

/// The unwrapped master key; wiped when dropped.
#[derive(Clone)]
pub struct MasterKey(Zeroizing<[u8; KEY_LEN]>);

impl MasterKey {
    pub fn generate() -> Result<Self, String> {
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        random(&mut key[..])?;
        Ok(MasterKey(key))
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let array: [u8; KEY_LEN] = bytes.try_into().ok()?;
        Some(MasterKey(Zeroizing::new(array)))
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0[..]
    }
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey(..)")
    }
}

pub fn random(buffer: &mut [u8]) -> Result<(), String> {
    getrandom::getrandom(buffer).map_err(|e| format!("no system randomness: {e}"))
}

pub fn random_vec(len: usize) -> Result<Vec<u8>, String> {
    let mut v = vec![0u8; len];
    random(&mut v)?;
    Ok(v)
}

pub fn b64(data: &[u8]) -> String {
    B64.encode(data)
}

pub fn unb64(text: &str) -> Option<Vec<u8>> {
    B64.decode(text).ok()
}

/// Argon2id parameters stored next to the wrapped key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kdf {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
    pub salt: Vec<u8>,
}

impl Kdf {
    pub fn new() -> Result<Self, String> {
        Ok(Kdf { memory_kib: 64 * 1024, iterations: 3, parallelism: 1, salt: random_vec(16)? })
    }

    /// Cheap parameters for unit tests only.
    #[cfg(test)]
    pub fn for_tests() -> Self {
        Kdf { memory_kib: 64, iterations: 1, parallelism: 1, salt: vec![7; 16] }
    }

    fn derive(&self, password: &[u8]) -> Result<Zeroizing<[u8; KEY_LEN]>, String> {
        let params = Params::new(self.memory_kib, self.iterations, self.parallelism, Some(KEY_LEN))
            .map_err(|e| format!("bad key derivation parameters: {e}"))?;
        let mut out = Zeroizing::new([0u8; KEY_LEN]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(password, &self.salt, &mut out[..])
            .map_err(|e| format!("key derivation failed: {e}"))?;
        Ok(out)
    }
}

/// `(nonce, ciphertext)` of `plain` under `key` with associated data `aad`.
pub fn seal(key: &[u8], aad: &[u8], plain: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| "bad key length".to_string())?;
    let nonce = random_vec(NONCE_LEN)?;
    let sealed = cipher.encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad })
        .map_err(|_| "encryption failed".to_string())?;
    Ok((nonce, sealed))
}

/// The plaintext, or `None` when the key is wrong or the data was changed.
pub fn open(key: &[u8], aad: &[u8], nonce: &[u8], sealed: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    if nonce.len() != NONCE_LEN {
        return None;
    }
    let cipher = XChaCha20Poly1305::new_from_slice(key).ok()?;
    cipher.decrypt(XNonce::from_slice(nonce), Payload { msg: sealed, aad }).ok().map(Zeroizing::new)
}

const WRAP_AAD: &[u8] = b"waykeyring v1 master key";

/// The master key wrapped with a password.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wrapped {
    pub kdf: Kdf,
    pub nonce: Vec<u8>,
    pub sealed: Vec<u8>,
}

impl Wrapped {
    pub fn wrap(master: &MasterKey, password: &[u8], kdf: Kdf) -> Result<Self, String> {
        let mut key = kdf.derive(password)?;
        let (nonce, sealed) = seal(&key[..], WRAP_AAD, master.bytes())?;
        key.zeroize();
        Ok(Wrapped { kdf, nonce, sealed })
    }

    /// `None` for a wrong password.
    pub fn unwrap_key(&self, password: &[u8]) -> Result<Option<MasterKey>, String> {
        let key = self.kdf.derive(password)?;
        Ok(open(&key[..], WRAP_AAD, &self.nonce, &self.sealed).and_then(|plain| MasterKey::from_bytes(&plain)))
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "version": 1,
            "kdf": {"algorithm": "argon2id", "memory_kib": self.kdf.memory_kib, "iterations": self.kdf.iterations,
                    "parallelism": self.kdf.parallelism, "salt": b64(&self.kdf.salt)},
            "nonce": b64(&self.nonce),
            "wrapped": b64(&self.sealed),
        })
    }

    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        let kdf = value.get("kdf")?;
        if kdf.get("algorithm")?.as_str()? != "argon2id" || value.get("version")?.as_u64()? != 1 {
            return None;
        }
        let number = |key: &str| kdf.get(key).and_then(|v| v.as_u64()).and_then(|v| u32::try_from(v).ok());
        Some(Wrapped {
            kdf: Kdf { memory_kib: number("memory_kib")?, iterations: number("iterations")?,
                       parallelism: number("parallelism")?, salt: unb64(kdf.get("salt")?.as_str()?)? },
            nonce: unb64(value.get("nonce")?.as_str()?)?,
            sealed: unb64(value.get("wrapped")?.as_str()?)?,
        })
    }
}

/// Index entry of one attribute.
pub fn attribute_hash(salt: &[u8], name: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(name.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    b64(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_round_trip_and_wrong_password() {
        let master = MasterKey::generate().unwrap();
        let wrapped = Wrapped::wrap(&master, b"hunter2", Kdf::for_tests()).unwrap();
        let back = Wrapped::from_json(&wrapped.to_json()).unwrap();
        assert_eq!(back, wrapped);
        assert_eq!(back.unwrap_key(b"hunter2").unwrap().unwrap().bytes(), master.bytes());
        assert!(back.unwrap_key(b"hunter3").unwrap().is_none());
    }

    #[test]
    fn sealed_data_is_bound_to_key_and_context() {
        let key = [3u8; KEY_LEN];
        let (nonce, sealed) = seal(&key, b"a", b"secret").unwrap();
        assert_eq!(open(&key, b"a", &nonce, &sealed).unwrap().as_slice(), b"secret");
        assert!(open(&key, b"b", &nonce, &sealed).is_none());
        assert!(open(&[4u8; KEY_LEN], b"a", &nonce, &sealed).is_none());
        let mut tampered = sealed.clone();
        tampered[0] ^= 1;
        assert!(open(&key, b"a", &nonce, &tampered).is_none());
    }

    #[test]
    fn attribute_hashes_depend_on_salt_and_pair() {
        let a = attribute_hash(b"s1", "service", "github");
        assert_eq!(a, attribute_hash(b"s1", "service", "github"));
        assert_ne!(a, attribute_hash(b"s2", "service", "github"));
        assert_ne!(attribute_hash(b"s1", "ab", "c"), attribute_hash(b"s1", "a", "bc"));
    }
}
