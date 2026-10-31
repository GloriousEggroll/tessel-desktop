//! The PAM helper side (`waykeyring --login`, `--change-password`), run as the
//! user by pam_waykeyring.so with the password on stdin.
//!
//! `--login`: first sign-in creates the keyring (master key wrapped with the
//! login password); later ones unwrap the master key. Either way the key goes
//! into the kernel's user keyring, where the daemon picks it up.
//! `--change-password`: stdin is `old NUL new`; the master key is re-wrapped.
use crate::store::Keyring;
use std::io::Read;
use std::path::PathBuf;
use zeroize::Zeroizing;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Created,
    Unlocked,
    WrongPassword,
    Changed,
}

pub fn read_stdin() -> Zeroizing<Vec<u8>> {
    let mut data = Zeroizing::new(Vec::new());
    let _ = std::io::stdin().take(64 * 1024).read_to_end(&mut data);
    data
}

/// `kernel`: also hand the master key to the kernel keyring (off in tests).
pub fn login(dir: PathBuf, password: &[u8], kernel: bool) -> Result<Outcome, String> {
    if password.is_empty() {
        return Err("no password (passwordless or fingerprint login); the keyring stays locked".into());
    }
    let mut keyring = Keyring::open(dir);
    let (outcome, master) = if keyring.is_set_up() {
        match keyring.master_for(password)? {
            Some(master) => (Outcome::Unlocked, master),
            None => return Ok(Outcome::WrongPassword),
        }
    } else {
        keyring.set_up(password)?;
        (Outcome::Created, keyring.master().cloned().ok_or("set up without a key")?)
    };
    if kernel {
        crate::kernel::store(&master)?;
    }
    Ok(outcome)
}

pub fn change_password(dir: PathBuf, input: &[u8]) -> Result<Outcome, String> {
    let split = input.iter().position(|b| *b == 0).ok_or("expected old NUL new on stdin")?;
    let (old, new) = (&input[..split], &input[split + 1..]);
    let mut keyring = Keyring::open(dir);
    if !keyring.is_set_up() {
        return Ok(Outcome::Changed);
    }
    if new.is_empty() {
        return Err("refusing an empty keyring password".into());
    }
    Ok(if keyring.change_password(old, new)? { Outcome::Changed } else { Outcome::WrongPassword })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_creates_then_unlocks_then_password_changes() {
        let dir = std::env::temp_dir().join(format!("waykeyring-login-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(login(dir.clone(), b"", false).is_err());
        assert_eq!(login(dir.clone(), b"pw", false).unwrap(), Outcome::Created);
        assert_eq!(login(dir.clone(), b"pw", false).unwrap(), Outcome::Unlocked);
        assert_eq!(login(dir.clone(), b"nope", false).unwrap(), Outcome::WrongPassword);
        assert_eq!(change_password(dir.clone(), b"bad\0new").unwrap(), Outcome::WrongPassword);
        assert_eq!(change_password(dir.clone(), b"pw\0new").unwrap(), Outcome::Changed);
        assert_eq!(login(dir.clone(), b"new", false).unwrap(), Outcome::Unlocked);
        let _ = std::fs::remove_dir_all(dir);
    }
}
