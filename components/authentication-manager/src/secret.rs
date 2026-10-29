//! A NUL-terminated byte buffer that is overwritten with zeros when cleared or
//! dropped. Used for PAM responses and polkit cookies on their way from the
//! GTK entry / libpolkit to `PolkitAgentSession`. It never reallocates (so no
//! stale copies are left behind by growth) and has no `Debug`/`Display`.

use std::ffi::CStr;
use std::sync::atomic::{compiler_fence, Ordering};

pub struct Secret(Vec<u8>);

impl Secret {
    pub fn empty() -> Self {
        Self::from_bytes(b"")
    }

    /// Copy `bytes` (without NUL; an interior NUL truncates) into a buffer of
    /// exactly the right capacity.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let bytes = match bytes.iter().position(|b| *b == 0) {
            Some(end) => &bytes[..end],
            None => bytes,
        };
        let mut buffer = Vec::with_capacity(bytes.len() + 1);
        buffer.extend_from_slice(bytes);
        buffer.push(0);
        Self(buffer)
    }

    /// Copy a C string without an intermediate Rust `String`.
    ///
    /// # Safety
    /// `pointer` must be NULL or a valid NUL-terminated string.
    pub unsafe fn from_ptr(pointer: *const std::os::raw::c_char) -> Self {
        if pointer.is_null() {
            return Self::empty();
        }
        Self::from_bytes(CStr::from_ptr(pointer).to_bytes())
    }

    pub fn as_c_str(&self) -> &CStr {
        CStr::from_bytes_with_nul(&self.0).unwrap_or_default()
    }

    /// The text (lossy only for invalid UTF-8, which GTK never produces).
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0[..self.0.len() - 1]).unwrap_or("")
    }

    pub fn is_empty(&self) -> bool {
        self.0.len() <= 1
    }

    /// Zero the contents and become empty.
    pub fn clear(&mut self) {
        wipe(&mut self.0);
        self.0.truncate(1);
    }
}

impl Clone for Secret {
    fn clone(&self) -> Self {
        Self::from_bytes(&self.0[..self.0.len() - 1])
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

fn wipe(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        // SAFETY: a valid &mut u8; volatile so the store is not elided.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_zeroes_and_keeps_terminator() {
        let mut secret = Secret::from_bytes(b"hunter2");
        assert_eq!(secret.as_str(), "hunter2");
        assert_eq!(secret.as_c_str().to_bytes(), b"hunter2");
        let pointer = secret.0.as_ptr();
        secret.clear();
        assert!(secret.is_empty());
        assert_eq!(secret.as_c_str().to_bytes(), b"");
        // The same allocation was overwritten in place.
        let bytes = unsafe { std::slice::from_raw_parts(pointer, 8) };
        assert_eq!(bytes, &[0u8; 8]);
    }

    #[test]
    fn interior_nul_truncates() {
        assert_eq!(Secret::from_bytes(b"ab\0cd").as_str(), "ab");
        assert!(unsafe { Secret::from_ptr(std::ptr::null()) }.is_empty());
    }
}
