//! Python `secrets` subset.

/// `secrets.token_urlsafe(n)`: `n` random bytes (getrandom), URL-safe base64 without padding.
pub fn token_urlsafe(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    let mut filled = 0;
    while filled < bytes {
        // SAFETY: writes at most `bytes - filled` bytes into the owned buffer.
        let result = unsafe { libc::getrandom(buffer[filled..].as_mut_ptr().cast(), bytes - filled, 0) };
        if result > 0 {
            filled += result as usize;
        } else if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            panic!("getrandom failed");
        }
    }
    crate::base64::encode_urlsafe_unpadded(&buffer)
}

/// Constant-time comparison (`secrets.compare_digest` for str).
pub fn compare_digest(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut difference = (a.len() != b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        difference |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_urlsafe_and_32_chars() {
        let token = token_urlsafe(24);
        assert_eq!(token.len(), 32);
        assert!(token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(token, token_urlsafe(24));
        assert!(compare_digest("abc", "abc"));
        assert!(!compare_digest("abc", "abd"));
        assert!(!compare_digest("abc", "abcd"));
    }
}
