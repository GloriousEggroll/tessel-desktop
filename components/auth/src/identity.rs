//! Polkit identities as the listener hands them over
//! (`polkit_identity_to_string`), their labels, and PAM prompt classification.

use std::ffi::{CStr, CString};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The original identity string; sessions are created from it with
    /// `polkit_identity_from_string`, exactly like Python's round trip.
    pub text: String,
    /// `Some(uid)` for a `PolkitUnixUser`.
    pub uid: Option<u32>,
}

impl Identity {
    /// `Polkit.identity_from_string` for `unix-user:<name|uid>`; any other kind
    /// keeps only its string form.
    pub fn parse(text: &str) -> Self {
        let uid = text.strip_prefix("unix-user:").and_then(|value| {
            value.parse::<u32>().ok().or_else(|| user_by_name(value).map(|user| user.uid))
        });
        Self { text: text.to_owned(), uid }
    }

    pub fn unix_user(uid: u32) -> Self {
        let text = match user_by_uid(uid) {
            Some(user) => format!("unix-user:{}", user.name),
            None => format!("unix-user:{uid}"),
        };
        Self { text, uid: Some(uid) }
    }

    /// `identity_name`: `"<GECOS name> (<login>)"`, the login, or the uid
    /// number for an unknown user; other identities use their string form.
    pub fn label(&self) -> String {
        match self.uid {
            Some(uid) => match user_by_uid(uid) {
                Some(user) => {
                    let real = user.gecos.split(',').next().unwrap_or("");
                    if !real.is_empty() && real != user.name { format!("{real} ({})", user.name) } else { user.name }
                }
                None => uid.to_string(),
            },
            None => self.text.clone(),
        }
    }
}

pub struct User {
    pub uid: u32,
    pub name: String,
    pub gecos: String,
}

fn lookup(call: impl Fn(*mut libc::passwd, *mut libc::c_char, usize, *mut *mut libc::passwd) -> libc::c_int) -> Option<User> {
    let mut buffer = vec![0 as libc::c_char; 16384];
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    if call(&mut entry, buffer.as_mut_ptr(), buffer.len(), &mut result) != 0 || result.is_null() {
        return None;
    }
    let text = |pointer: *const libc::c_char| {
        if pointer.is_null() { String::new() } else { unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned() }
    };
    Some(User { uid: entry.pw_uid, name: text(entry.pw_name), gecos: text(entry.pw_gecos) })
}

pub fn user_by_uid(uid: u32) -> Option<User> {
    lookup(|entry, buffer, size, result| unsafe { libc::getpwuid_r(uid, entry, buffer, size, result) })
}

pub fn user_by_name(name: &str) -> Option<User> {
    let name = CString::new(name).ok()?;
    lookup(|entry, buffer, size, result| unsafe { libc::getpwnam_r(name.as_ptr(), entry, buffer, size, result) })
}

pub fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

/// Python `str.casefold` for the cases that matter for "Password" in the 31
/// catalog languages (lowercase plus the ß/ς folds).
pub fn casefold(text: &str) -> String {
    text.to_lowercase().replace('ß', "ss").replace('ς', "σ")
}

/// True for PAM's plain password prompt ("Password:"), in English or translated
/// (`translated` = `tr("Password")`).
pub fn is_password_prompt_with(prompt: &str, translated: &str) -> bool {
    let text = prompt.trim().trim_end_matches([':', '：']).trim();
    let text = casefold(text);
    text == "password" || text == casefold(translated)
}

pub fn is_password_prompt(prompt: &str) -> bool {
    is_password_prompt_with(prompt, &tessel_core::tr!("Password"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_prompts() {
        assert!(is_password_prompt("Password: "));
        assert!(is_password_prompt("  password："));
        assert!(is_password_prompt("PASSWORD::"));
        assert!(!is_password_prompt("Verification code:"));
        assert!(!is_password_prompt(""));
        assert!(is_password_prompt_with("Passwort:", "Passwort"));
        assert!(is_password_prompt_with("Mot de passe :", "Mot de passe"));
        assert!(!is_password_prompt_with("Passwort:", "Password"));
    }

    #[test]
    fn identities_parse_and_label() {
        let root = Identity::parse("unix-user:0");
        assert_eq!(root.uid, Some(0));
        assert_eq!(Identity::parse("unix-user:root").uid, Some(0));
        let user = user_by_uid(0).unwrap();
        let real = user.gecos.split(',').next().unwrap_or("").to_owned();
        assert_eq!(root.label(), if !real.is_empty() && real != "root" { format!("{real} (root)") } else { "root".into() });
        let group = Identity::parse("unix-group:wheel");
        assert_eq!(group.uid, None);
        assert_eq!(group.label(), "unix-group:wheel");
        // Unknown uid: the number.
        assert_eq!(Identity::parse("unix-user:3999999").label(), "3999999");
        let me = Identity::unix_user(current_uid());
        assert_eq!(Identity::parse(&me.text).uid, Some(current_uid()));
        let user = user_by_uid(current_uid()).unwrap();
        let real = user.gecos.split(',').next().unwrap_or("").to_owned();
        let expected = if !real.is_empty() && real != user.name { format!("{real} ({})", user.name) } else { user.name };
        assert_eq!(me.label(), expected);
    }
}
