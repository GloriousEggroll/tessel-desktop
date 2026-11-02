//! Import of KDE wallets (`~/.local/share/kwalletd/<name>.kwl` + `<name>.salt`),
//! following KWallet's backend: header "KWALLET\n\r\0\r\n", version bytes
//! (0, 1, cipher, hash); a list of MD5 folder/key hashes; then Blowfish
//! (448-bit key = PBKDF2-SHA512(password, salt, 50000)) CBC with a zero IV, or
//! the legacy ECB mode, of: one random block, the length (u32 BE), a
//! QDataStream of folders and entries, padding, and SHA-1 of the stream.
//! Entries: Password (QString), Stream (bytes), Map (QMap<QString, QString>).
//! `<name>_attributes.json` (KWallet's Secret Service layer) supplies the
//! attributes applications search for.
use crate::gnome_import::Imported;
use blowfish::cipher::{BlockDecrypt, KeyInit};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

const MAGIC: &[u8] = b"KWALLET\n\r\0\r\n";
const PORTAL_SCHEMA: &str = "org.freedesktop.impl.portal.Secret";

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    WrongPassword,
    Unsupported(String),
    Damaged(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::WrongPassword => f.write_str("wrong wallet password"),
            Error::Unsupported(what) => write!(f, "unsupported wallet: {what}"),
            Error::Damaged(what) => write!(f, "damaged wallet: {what}"),
        }
    }
}

pub fn kwallet_dir() -> PathBuf {
    tessel_core::paths::data_home().join("kwalletd")
}

/// Wallet files (`*.kwl`) in KWallet's directory.
pub fn wallets(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "kwl")).collect();
    files.sort();
    files
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.at.checked_add(n).filter(|e| *e <= self.data.len())
            .ok_or_else(|| Error::Damaged("truncated".into()))?;
        let slice = &self.data[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn done(&self) -> bool {
        self.at >= self.data.len()
    }

    /// QDataStream QString: byte length (0xffffffff = null) + UTF-16BE.
    fn qstring(&mut self) -> Result<String, Error> {
        let len = self.u32()?;
        if len == u32::MAX {
            return Ok(String::new());
        }
        let bytes = self.take(len as usize)?;
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        Ok(String::from_utf16_lossy(&units))
    }

    fn qbytearray(&mut self) -> Result<Vec<u8>, Error> {
        let len = self.u32()?;
        if len == u32::MAX {
            return Ok(Vec::new());
        }
        Ok(self.take(len as usize)?.to_vec())
    }
}

pub struct Entry {
    pub folder: String,
    pub key: String,
    /// 1 Password, 2 Stream, 3 Map
    pub kind: i32,
    pub value: Zeroizing<Vec<u8>>,
}

/// Decrypt and parse a wallet.
pub fn read(wallet: &[u8], salt: Option<&[u8]>, password: &[u8]) -> Result<Vec<Entry>, Error> {
    if wallet.len() < 16 || &wallet[..12] != MAGIC {
        return Err(Error::Damaged("not a KWallet file".into()));
    }
    let (major, minor, cipher, hash) = (wallet[12], wallet[13], wallet[14], wallet[15]);
    if major != 0 || minor != 1 {
        return Err(Error::Unsupported(format!("version {major}.{minor}")));
    }
    let ecb = match cipher {
        0 => true,
        3 => false,
        2 => return Err(Error::Unsupported("GPG-encrypted wallet".into())),
        other => return Err(Error::Unsupported(format!("cipher {other}"))),
    };
    if hash != 2 {
        return Err(Error::Unsupported("wallet from before KDE 4.13 (SHA-1 password hash); open it once in KWallet".into()));
    }
    let salt = salt.filter(|s| !s.is_empty()).ok_or_else(|| Error::Damaged("the .salt file is missing".into()))?;
    let mut key = Zeroizing::new([0u8; 56]);
    pbkdf2::pbkdf2_hmac::<sha2::Sha512>(password, salt, 50_000, &mut key[..]);

    let mut reader = Reader { data: &wallet[16..], at: 0 };
    let folders = reader.u32()?;
    if folders > 0xffff {
        return Err(Error::Damaged("bad hash table".into()));
    }
    for _ in 0..folders {
        reader.take(16)?;
        let entries = reader.u32()?;
        reader.take(16usize.checked_mul(entries as usize).ok_or_else(|| Error::Damaged("bad hash table".into()))?)?;
    }
    let encrypted = &reader.data[reader.at..];
    if encrypted.is_empty() || encrypted.len() % 8 != 0 || encrypted.len() < 8 + 4 + 20 {
        return Err(Error::Damaged("bad encrypted size".into()));
    }
    let cipher = <blowfish::Blowfish>::new_from_slice(&key[..])
        .map_err(|_| Error::Damaged("bad key".into()))?;
    let mut plain = Zeroizing::new(encrypted.to_vec());
    let mut previous = [0u8; 8];
    for block in plain.chunks_exact_mut(8) {
        let saved: [u8; 8] = block.try_into().unwrap();
        cipher.decrypt_block(block.into());
        if !ecb {
            for (b, p) in block.iter_mut().zip(previous) {
                *b ^= p;
            }
            previous = saved;
        }
    }
    let size = u32::from_be_bytes(plain[8..12].try_into().unwrap()) as usize;
    if size > plain.len() - 8 - 4 - 20 {
        return Err(Error::WrongPassword);
    }
    let stream = &plain[12..12 + size];
    if Sha1::digest(stream).as_slice() != &plain[plain.len() - 20..] {
        return Err(Error::WrongPassword);
    }
    let mut data = Reader { data: stream, at: 0 };
    let mut entries = Vec::new();
    while !data.done() {
        let folder = data.qstring()?;
        let count = data.u32()?;
        for _ in 0..count {
            let key = data.qstring()?;
            let kind = data.u32()? as i32;
            let value = Zeroizing::new(data.qbytearray()?);
            if (1..=3).contains(&kind) {
                entries.push(Entry { folder: folder.clone(), key, kind, value });
            }
        }
    }
    Ok(entries)
}

/// The secret bytes and content type of an entry.
fn secret_of(entry: &Entry) -> Option<(Zeroizing<Vec<u8>>, &'static str)> {
    let mut r = Reader { data: &entry.value, at: 0 };
    match entry.kind {
        1 => Some((Zeroizing::new(r.qstring().ok()?.into_bytes()), "text/plain")),
        2 => Some((entry.value.clone(), "application/octet-stream")),
        3 => {
            let n = r.u32().ok()?;
            let mut map = serde_json::Map::new();
            for _ in 0..n {
                let k = r.qstring().ok()?;
                let v = r.qstring().ok()?;
                map.insert(k, serde_json::Value::String(v));
            }
            Some((Zeroizing::new(serde_json::Value::Object(map).to_string().into_bytes()), "application/json"))
        }
        _ => None,
    }
}

/// Entries as items: attributes from the wallet's `_attributes.json` (what
/// applications searched for under KDE), Flatpak portal keys under the
/// portal's own attributes, anything else under kwallet-folder/kwallet-key.
pub fn to_items(entries: &[Entry], attributes_json: Option<&serde_json::Value>) -> Vec<Imported> {
    let mut items = Vec::new();
    for entry in entries {
        let Some((secret, _content_type)) = secret_of(entry) else { continue };
        let known = attributes_json.and_then(|j| j.get(format!("{}/{}", entry.folder, entry.key)))
            .and_then(|e| e.get("attributes")).and_then(|a| a.as_object());
        let attributes: BTreeMap<String, String> = if entry.folder == "xdg-desktop-portal" && !entry.key.is_empty() {
            [("app_id".to_owned(), entry.key.clone()), ("xdg:schema".to_owned(), PORTAL_SCHEMA.to_owned())].into()
        } else if let Some(known) = known {
            known.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect()
        } else {
            [("kwallet-folder".to_owned(), entry.folder.clone()), ("kwallet-key".to_owned(), entry.key.clone())].into()
        };
        let label = if entry.folder == "xdg-desktop-portal" {
            format!("Application key for {}", entry.key)
        } else {
            entry.key.clone()
        };
        items.push(Imported { label, attributes, secret });
    }
    items
}

/// Read `<wallet>.kwl` with its `.salt` and `_attributes.json` siblings.
pub fn import_file(path: &Path, password: &[u8]) -> Result<Vec<Imported>, Error> {
    let data = std::fs::read(path).map_err(|e| Error::Damaged(e.to_string()))?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_owned();
    let dir = path.parent().unwrap_or(Path::new("."));
    let salt = std::fs::read(dir.join(format!("{stem}.salt"))).ok();
    let attributes = std::fs::read(dir.join(format!("{stem}_attributes.json"))).ok()
        .and_then(|d| serde_json::from_slice::<serde_json::Value>(&d).ok());
    let entries = read(&data, salt.as_deref(), password)?;
    Ok(to_items(&entries, attributes.as_ref()))
}

// KWallet byte-swaps each word on little-endian hosts: that is standard
// big-endian Blowfish, RustCrypto's default.

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use blowfish::cipher::BlockEncrypt;

    fn qstring(s: &str) -> Vec<u8> {
        let units: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        let mut out = (units.len() as u32).to_be_bytes().to_vec();
        out.extend(units);
        out
    }

    fn qbytes(b: &[u8]) -> Vec<u8> {
        let mut out = (b.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(b);
        out
    }

    /// A wallet written the way KWallet's BlowfishPersistHandler::write does (CBC, PBKDF2).
    pub(crate) fn make_wallet(password: &[u8], salt: &[u8], ecb: bool) -> Vec<u8> {
        let mut stream = Vec::new();
        stream.extend(qstring("Passwords"));
        stream.extend(2u32.to_be_bytes());
        stream.extend(qstring("github"));
        stream.extend(1u32.to_be_bytes());
        stream.extend(qbytes(&qstring("tøken")));
        stream.extend(qstring("form"));
        stream.extend(3u32.to_be_bytes());
        let mut map = 1u32.to_be_bytes().to_vec();
        map.extend(qstring("user"));
        map.extend(qstring("me"));
        stream.extend(qbytes(&map));
        stream.extend(qstring("xdg-desktop-portal"));
        stream.extend(1u32.to_be_bytes());
        stream.extend(qstring("org.kde.Imported"));
        stream.extend(2u32.to_be_bytes());
        stream.extend(qbytes(&[1, 2, 3]));
        let mut plain = vec![9u8; 8];
        plain.extend((stream.len() as u32).to_be_bytes());
        plain.extend(&stream);
        let hash = Sha1::digest(&stream);
        while (plain.len() + 20) % 8 != 0 {
            plain.push(0);
        }
        plain.extend(hash);
        let mut key = [0u8; 56];
        pbkdf2::pbkdf2_hmac::<sha2::Sha512>(password, salt, 50_000, &mut key);
        let cipher = <blowfish::Blowfish>::new_from_slice(&key).unwrap();
        let mut previous = [0u8; 8];
        for block in plain.chunks_exact_mut(8) {
            if !ecb {
                for (b, p) in block.iter_mut().zip(previous) {
                    *b ^= p;
                }
            }
            cipher.encrypt_block(block.into());
            previous = block.try_into().unwrap();
        }
        let mut file = MAGIC.to_vec();
        file.extend([0, 1, if ecb { 0 } else { 3 }, 2]);
        file.extend(0u32.to_be_bytes());
        file.extend(plain);
        file
    }

    #[test]
    fn standard_blowfish_test_vector() {
        // Schneier's vector: key 0000000000000000, plaintext 0000000000000000 -> 4EF997456198DD78.
        let cipher = <blowfish::Blowfish>::new_from_slice(&[0u8; 8]).unwrap();
        let mut block = [0u8; 8];
        cipher.encrypt_block((&mut block).into());
        assert_eq!(block, [0x4e, 0xf9, 0x97, 0x45, 0x61, 0x98, 0xdd, 0x78]);
    }

    #[test]
    fn reads_cbc_and_ecb_wallets_and_detects_wrong_passwords() {
        for ecb in [false, true] {
            let wallet = make_wallet(b"pw", &[5u8; 56], ecb);
            assert_eq!(read(&wallet, Some(&[5u8; 56]), b"bad").err(), Some(Error::WrongPassword));
            let entries = read(&wallet, Some(&[5u8; 56]), b"pw").unwrap();
            let items = to_items(&entries, Some(&serde_json::json!({
                "Passwords/github": {"attributes": {"service": "github", "user": "me"}}
            })));
            assert_eq!(items.len(), 3);
            assert_eq!(items[0].secret.as_slice(), "tøken".as_bytes());
            assert_eq!(items[0].attributes.get("service").map(String::as_str), Some("github"));
            assert_eq!(items[1].secret.as_slice(), br#"{"user":"me"}"#);
            assert_eq!(items[1].attributes.get("kwallet-key").map(String::as_str), Some("form"));
            assert_eq!(items[2].secret.as_slice(), &[1, 2, 3]);
            assert_eq!(items[2].attributes.get("app_id").map(String::as_str), Some("org.kde.Imported"));
        }
        assert!(matches!(read(b"nope", None, b""), Err(Error::Damaged(_))));
    }
}

#[cfg(test)]
mod probe {
    /// `KWALLET_PROBE=<file.kwl> cargo test -p tessel-keyring probe_real_wallet`: the
    /// structure parses and a wrong password is reported as such (no secrets read).
    #[test]
    fn probe_real_wallet() {
        let Ok(path) = std::env::var("KWALLET_PROBE") else { return };
        let path = std::path::PathBuf::from(path);
        let result = super::import_file(&path, b"\x01definitely-not-the-password");
        assert_eq!(result.err(), Some(super::Error::WrongPassword));
    }
}

#[cfg(test)]
mod fixtures {
    /// `WAYKEYRING_WRITE_FIXTURES=1 cargo test -p tessel-keyring write_kwallet_fixtures`
    /// regenerates tests/fixtures/kwalletd (wallets "kdewallet" = "pw", "old" = "kpw").
    #[test]
    fn write_kwallet_fixtures() {
        if std::env::var("WAYKEYRING_WRITE_FIXTURES").is_err() {
            return;
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kwalletd");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, password, salt) in [("kdewallet", b"pw".as_slice(), [3u8; 56]), ("old", b"kpw".as_slice(), [4u8; 56])] {
            std::fs::write(dir.join(format!("{name}.kwl")), super::tests::make_wallet(password, &salt, false)).unwrap();
            std::fs::write(dir.join(format!("{name}.salt")), salt).unwrap();
        }
        std::fs::write(dir.join("kdewallet_attributes.json"),
                       r#"{"Passwords/github": {"attributes": {"service": "kde-github", "user": "kde-me"}}}"#).unwrap();
    }
}
