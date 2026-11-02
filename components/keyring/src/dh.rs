//! Secret Service transport encryption "dh-ietf1024-sha256-aes128-cbc-pkcs7"
//! (what libsecret asks for first): RFC 2409 group 2, HKDF-SHA256 (no salt,
//! no info) of the zero-padded shared secret to an AES-128 key; secrets are
//! AES-128-CBC with PKCS#7 padding and a random IV in the parameters.
use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use num_bigint::BigUint;
use zeroize::Zeroizing;

pub const ALGORITHM: &str = "dh-ietf1024-sha256-aes128-cbc-pkcs7";
const PRIME_HEX: &str = "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DD\
EF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1FE6\
49286651ECE65381FFFFFFFFFFFFFFFF";
const PRIME_LEN: usize = 128;

type Enc = cbc::Encryptor<aes::Aes128>;
type Dec = cbc::Decryptor<aes::Aes128>;

fn prime() -> BigUint {
    BigUint::parse_bytes(PRIME_HEX.as_bytes(), 16).unwrap()
}

fn padded(n: &BigUint) -> Vec<u8> {
    let bytes = n.to_bytes_be();
    let mut out = vec![0u8; PRIME_LEN.saturating_sub(bytes.len())];
    out.extend_from_slice(&bytes);
    out
}

/// Server side of the key exchange: (our public key, the AES key).
pub fn negotiate(client_public: &[u8]) -> Result<(Vec<u8>, Zeroizing<[u8; 16]>), String> {
    let p = prime();
    let client = BigUint::from_bytes_be(client_public);
    if client <= BigUint::from(1u8) || client >= &p - 1u8 {
        return Err("invalid public key".into());
    }
    let private = Zeroizing::new(crate::crypto::random_vec(PRIME_LEN)?);
    let x = BigUint::from_bytes_be(&private);
    let public = BigUint::from(2u8).modpow(&x, &p);
    let shared = Zeroizing::new(padded(&client.modpow(&x, &p)));
    let mut key = Zeroizing::new([0u8; 16]);
    hkdf::Hkdf::<sha2::Sha256>::new(None, &shared).expand(&[], &mut key[..]).map_err(|_| "hkdf failed".to_string())?;
    Ok((padded(&public), key))
}

/// (iv, ciphertext)
pub fn encrypt(key: &[u8; 16], plain: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let iv = crate::crypto::random_vec(16)?;
    let sealed = Enc::new(key.into(), iv.as_slice().into()).encrypt_padded_vec_mut::<Pkcs7>(plain);
    Ok((iv, sealed))
}

pub fn decrypt(key: &[u8; 16], iv: &[u8], sealed: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    if iv.len() != 16 {
        return None;
    }
    Dec::new(key.into(), iv.into()).decrypt_padded_vec_mut::<Pkcs7>(sealed).ok().map(Zeroizing::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_agree_and_round_trip() {
        let p = prime();
        let y = BigUint::from_bytes_be(&[5u8; 128]) % &p;
        let client_public = BigUint::from(2u8).modpow(&y, &p);
        let (server_public, key) = negotiate(&padded(&client_public)).unwrap();
        assert_eq!(server_public.len(), 128);
        let shared = padded(&BigUint::from_bytes_be(&server_public).modpow(&y, &p));
        let mut client_key = [0u8; 16];
        hkdf::Hkdf::<sha2::Sha256>::new(None, &shared).expand(&[], &mut client_key).unwrap();
        assert_eq!(&client_key, &*key);
        let (iv, sealed) = encrypt(&key, b"secret").unwrap();
        assert_eq!(decrypt(&client_key, &iv, &sealed).unwrap().as_slice(), b"secret");
        assert!(negotiate(&[1]).is_err());
    }
}
