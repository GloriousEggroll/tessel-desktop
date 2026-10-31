//! The unwrapped master key handed from the login (PAM helper) to the daemon
//! through the kernel's per-user keyring (`@u`, keyctl(2)): a "user" key named
//! [`DESCRIPTION`], readable only by the same uid.
use crate::crypto::MasterKey;

pub const DESCRIPTION: &str = "waykeyring:master";
const KEY_SPEC_USER_KEYRING: libc::c_long = -4;
const KEYCTL_UNLINK: libc::c_long = 9;
const KEYCTL_SEARCH: libc::c_long = 10;
const KEYCTL_READ: libc::c_long = 11;
const KEYCTL_SETPERM: libc::c_long = 5;
/// Possessor and user: view, read, write, search, link, setattr. Nobody else.
const PERMISSIONS: libc::c_long = 0x3f3f_0000;

fn cstr(text: &str) -> std::ffi::CString {
    std::ffi::CString::new(text).unwrap()
}

/// Store (or replace) the key in the calling uid's user keyring.
pub fn store(master: &MasterKey) -> Result<(), String> {
    store_as(DESCRIPTION, master)
}

fn store_as(name: &str, master: &MasterKey) -> Result<(), String> {
    let (kind, description) = (cstr("user"), cstr(name));
    let data = master.bytes();
    // SAFETY: valid NUL-terminated strings and a buffer of the given length.
    let id = unsafe {
        libc::syscall(libc::SYS_add_key, kind.as_ptr(), description.as_ptr(), data.as_ptr(), data.len(),
                      KEY_SPEC_USER_KEYRING)
    };
    if id < 0 {
        return Err(format!("add_key: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: plain keyctl call on the id just returned.
    if unsafe { libc::syscall(libc::SYS_keyctl, KEYCTL_SETPERM, id, PERMISSIONS) } < 0 {
        return Err(format!("keyctl setperm: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

fn find(name: &str) -> Option<libc::c_long> {
    let (kind, description) = (cstr("user"), cstr(name));
    // SAFETY: valid NUL-terminated strings.
    let id = unsafe {
        libc::syscall(libc::SYS_keyctl, KEYCTL_SEARCH, KEY_SPEC_USER_KEYRING, kind.as_ptr(), description.as_ptr(), 0)
    };
    (id >= 0).then_some(id)
}

/// The key the login stored, if any.
pub fn load() -> Option<MasterKey> {
    load_as(DESCRIPTION)
}

fn load_as(name: &str) -> Option<MasterKey> {
    let id = find(name)?;
    let mut buffer = zeroize::Zeroizing::new([0u8; 64]);
    // SAFETY: the buffer is valid for its length.
    let len = unsafe { libc::syscall(libc::SYS_keyctl, KEYCTL_READ, id, buffer.as_mut_ptr(), buffer.len()) };
    if len < 0 || len as usize > buffer.len() {
        return None;
    }
    MasterKey::from_bytes(&buffer[..len as usize])
}

/// Remove the key (keyring locked by the user).
pub fn forget() {
    forget_as(DESCRIPTION)
}

fn forget_as(name: &str) {
    if let Some(id) = find(name) {
        // SAFETY: plain keyctl call.
        unsafe { libc::syscall(libc::SYS_keyctl, KEYCTL_UNLINK, id, KEY_SPEC_USER_KEYRING) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_load_forget_in_the_user_keyring() {
        // A throw-away name: the real waykeyring key is never touched.
        let name = format!("waykeyring-test:{}", std::process::id());
        let key = MasterKey::generate().unwrap();
        store_as(&name, &key).unwrap();
        assert_eq!(load_as(&name).unwrap().bytes(), key.bytes());
        forget_as(&name);
        assert!(load_as(&name).is_none());
    }
}
