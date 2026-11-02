//! pam_tessel_keyring.so
//!
//! ```text
//! -auth     optional pam_tessel_keyring.so      # keep a copy of the password for the session
//! -session  optional pam_tessel_keyring.so      # run `tessel-keyring --login` as the user with it on stdin
//! -password optional pam_tessel_keyring.so      # run `tessel-keyring --change-password` (old NUL new)
//! ```
//!
//! Never fails a login: every entry point returns PAM_IGNORE (or PAM_SUCCESS
//! for setcred/close_session). The helper runs with the user's uid, gid and
//! groups, an empty environment (HOME, USER, LOGNAME, PATH) and the password
//! on a pipe. Option `helper=/path` overrides [`DEFAULT_HELPER`].
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;

pub const DEFAULT_HELPER: &str = "/usr/local/lib/tessel/tessel-keyring";
const DATA_NAME: &[u8] = b"tessel_keyring_authtok\0";
const HELPER_TIMEOUT_SECONDS: u64 = 20;

const PAM_SUCCESS: c_int = 0;
const PAM_IGNORE: c_int = 25;
const PAM_USER: c_int = 2;
const PAM_AUTHTOK: c_int = 6;
const PAM_OLDAUTHTOK: c_int = 7;
const PAM_UPDATE_AUTHTOK: c_int = 0x2000;

#[repr(C)]
pub struct PamHandle {
    _private: [u8; 0],
}

type Cleanup = extern "C" fn(*mut PamHandle, *mut c_void, c_int);

extern "C" {
    fn pam_get_item(pamh: *const PamHandle, item_type: c_int, item: *mut *const c_void) -> c_int;
    fn pam_set_data(pamh: *mut PamHandle, name: *const c_char, data: *mut c_void, cleanup: Option<Cleanup>) -> c_int;
    fn pam_get_data(pamh: *const PamHandle, name: *const c_char, data: *mut *const c_void) -> c_int;
}

/// Securely owned copy of the password stored as PAM data.
struct Secret(Vec<u8>);

impl Drop for Secret {
    fn drop(&mut self) {
        for byte in self.0.iter_mut() {
            // SAFETY: plain volatile write into owned memory so the wipe is not optimised away.
            unsafe { ptr::write_volatile(byte, 0) };
        }
    }
}

extern "C" fn free_secret(_: *mut PamHandle, data: *mut c_void, _: c_int) {
    if !data.is_null() {
        // SAFETY: created by Box::into_raw in store_authtok.
        drop(unsafe { Box::from_raw(data as *mut Secret) });
    }
}

fn item(pamh: *const PamHandle, kind: c_int) -> Option<Vec<u8>> {
    let mut value: *const c_void = ptr::null();
    // SAFETY: pam_get_item writes a pointer owned by libpam.
    if unsafe { pam_get_item(pamh, kind, &mut value) } != PAM_SUCCESS || value.is_null() {
        return None;
    }
    // SAFETY: string items are NUL-terminated C strings.
    Some(unsafe { CStr::from_ptr(value as *const c_char) }.to_bytes().to_vec())
}

fn helper_path(argc: c_int, argv: *const *const c_char) -> CString {
    let mut helper = DEFAULT_HELPER.to_owned();
    for i in 0..argc.max(0) as usize {
        // SAFETY: libpam passes argc valid C strings.
        let arg = unsafe { CStr::from_ptr(*argv.add(i)) }.to_string_lossy().into_owned();
        if let Some(path) = arg.strip_prefix("helper=") {
            helper = path.to_owned();
        }
    }
    CString::new(helper).unwrap_or_else(|_| CString::new(DEFAULT_HELPER).unwrap())
}

fn log(message: &str) {
    if let Ok(text) = CString::new(format!("pam_tessel_keyring: {message}")) {
        // SAFETY: constant format string and a valid C string argument.
        unsafe { libc::syslog(libc::LOG_AUTHPRIV | libc::LOG_NOTICE, b"%s\0".as_ptr() as *const c_char, text.as_ptr()) };
    }
}

struct User {
    name: CString,
    uid: libc::uid_t,
    gid: libc::gid_t,
    home: CString,
}

fn lookup_user(name: &[u8]) -> Option<User> {
    let cname = CString::new(name).ok()?;
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buffer = vec![0 as c_char; 16 * 1024];
    let mut result: *mut libc::passwd = ptr::null_mut();
    // SAFETY: getpwnam_r writes into the provided buffers.
    let rc = unsafe { libc::getpwnam_r(cname.as_ptr(), &mut pwd, buffer.as_mut_ptr(), buffer.len(), &mut result) };
    if rc != 0 || result.is_null() {
        return None;
    }
    // SAFETY: getpwnam_r filled pw_dir with a C string inside buffer.
    let home = unsafe { CStr::from_ptr(pwd.pw_dir) }.to_owned();
    Some(User { name: cname, uid: pwd.pw_uid, gid: pwd.pw_gid, home })
}

/// Run `helper mode` as `user` with `input` on stdin; true when it exited 0.
pub fn run_helper(helper: &CStr, mode: &str, user_name: &[u8], input: &[u8]) -> bool {
    let Some(user) = lookup_user(user_name) else {
        log("unknown user");
        return false;
    };
    if user.uid == 0 {
        return false;
    }
    let mode = CString::new(mode).unwrap();
    let argv = [helper.as_ptr(), mode.as_ptr(), ptr::null()];
    let env_strings: Vec<CString> = [
        format!("HOME={}", user.home.to_string_lossy()),
        format!("USER={}", user.name.to_string_lossy()),
        format!("LOGNAME={}", user.name.to_string_lossy()),
        "PATH=/usr/local/bin:/usr/bin:/bin".to_owned(),
    ].into_iter().filter_map(|s| CString::new(s).ok()).collect();
    let mut envp: Vec<*const c_char> = env_strings.iter().map(|s| s.as_ptr()).collect();
    envp.push(ptr::null());
    let mut fds = [0 as c_int; 2];
    // SAFETY: fds is a two-element array.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return false;
    }
    let (read_end, write_end) = (fds[0], fds[1]);
    // SAFETY: fork; the child only calls exec-preparation functions and _exit.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        unsafe {
            libc::close(read_end);
            libc::close(write_end);
        }
        return false;
    }
    if pid == 0 {
        // Child: become the user; stdin = the pipe, stdout = /dev/null, stderr kept for the journal.
        unsafe {
            libc::dup2(read_end, 0);
            let null = libc::open(b"/dev/null\0".as_ptr() as *const c_char, libc::O_WRONLY);
            if null >= 0 {
                libc::dup2(null, 1);
            }
            // As root (greetd, passwd): drop to the user. Otherwise we already are the user.
            let dropped = if libc::geteuid() == 0 {
                libc::initgroups(user.name.as_ptr(), user.gid) == 0 && libc::setgid(user.gid) == 0
                    && libc::setuid(user.uid) == 0
            } else {
                libc::getuid() == user.uid
            };
            if !dropped
                || libc::getuid() != user.uid
                || libc::geteuid() != user.uid
                || libc::setuid(0) == 0
            {
                libc::_exit(126);
            }
            libc::chdir(user.home.as_ptr());
            libc::execve(helper.as_ptr(), argv.as_ptr(), envp.as_ptr());
            libc::_exit(127);
        }
    }
    unsafe { libc::close(read_end) };
    let mut written = 0;
    while written < input.len() {
        // SAFETY: writing from a valid buffer.
        let n = unsafe { libc::write(write_end, input[written..].as_ptr() as *const c_void, input.len() - written) };
        if n <= 0 {
            break;
        }
        written += n as usize;
    }
    unsafe { libc::close(write_end) };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(HELPER_TIMEOUT_SECONDS);
    loop {
        let mut status = 0;
        // SAFETY: waiting for our own child.
        let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if rc == pid {
            return libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0;
        }
        if rc < 0 {
            return false;
        }
        if std::time::Instant::now() > deadline {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, &mut status, 0);
            }
            log("helper timed out");
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[no_mangle]
pub extern "C" fn pam_sm_authenticate(pamh: *mut PamHandle, _flags: c_int, _argc: c_int,
                                      _argv: *const *const c_char) -> c_int {
    if let Some(password) = item(pamh, PAM_AUTHTOK).filter(|p| !p.is_empty()) {
        let data = Box::into_raw(Box::new(Secret(password))) as *mut c_void;
        // SAFETY: libpam owns the data from now on and frees it with free_secret.
        if unsafe { pam_set_data(pamh, DATA_NAME.as_ptr() as *const c_char, data, Some(free_secret)) } != PAM_SUCCESS {
            free_secret(pamh, data, 0);
        }
    }
    PAM_IGNORE
}

#[no_mangle]
pub extern "C" fn pam_sm_setcred(_: *mut PamHandle, _: c_int, _: c_int, _: *const *const c_char) -> c_int {
    PAM_SUCCESS
}

#[no_mangle]
pub extern "C" fn pam_sm_open_session(pamh: *mut PamHandle, _flags: c_int, argc: c_int,
                                      argv: *const *const c_char) -> c_int {
    let mut data: *const c_void = ptr::null();
    // SAFETY: reads back the pointer stored by pam_sm_authenticate.
    if unsafe { pam_get_data(pamh, DATA_NAME.as_ptr() as *const c_char, &mut data) } != PAM_SUCCESS || data.is_null() {
        return PAM_IGNORE;
    }
    let Some(user) = item(pamh, PAM_USER) else { return PAM_IGNORE };
    // SAFETY: data is the Secret stored by pam_sm_authenticate and lives until pam_end.
    let secret = unsafe { &*(data as *const Secret) };
    if !run_helper(&helper_path(argc, argv), "--login", &user, &secret.0) {
        log("the keyring was not unlocked for this session");
    }
    // The copy is no longer needed: replacing the data frees (and wipes) it.
    // SAFETY: a null replacement; libpam calls free_secret on the old data.
    unsafe { pam_set_data(pamh, DATA_NAME.as_ptr() as *const c_char, ptr::null_mut(), None) };
    PAM_IGNORE
}

#[no_mangle]
pub extern "C" fn pam_sm_close_session(_: *mut PamHandle, _: c_int, _: c_int, _: *const *const c_char) -> c_int {
    PAM_SUCCESS
}

#[no_mangle]
pub extern "C" fn pam_sm_chauthtok(pamh: *mut PamHandle, flags: c_int, argc: c_int,
                                   argv: *const *const c_char) -> c_int {
    if flags & PAM_UPDATE_AUTHTOK == 0 {
        return PAM_IGNORE;
    }
    let (Some(user), Some(old), Some(new)) = (item(pamh, PAM_USER), item(pamh, PAM_OLDAUTHTOK), item(pamh, PAM_AUTHTOK))
    else {
        return PAM_IGNORE;
    };
    let mut input = Secret(Vec::with_capacity(old.len() + new.len() + 1));
    input.0.extend_from_slice(&old);
    input.0.push(0);
    input.0.extend_from_slice(&new);
    drop((Secret(old), Secret(new)));
    if !run_helper(&helper_path(argc, argv), "--change-password", &user, &input.0) {
        log("the keyring password was not changed");
    }
    PAM_IGNORE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_receives_stdin_as_the_same_user() {
        // As a normal user, the setuid to ourselves succeeds and setuid(0) fails: the same path as under root.
        let dir = std::env::temp_dir().join(format!("pam-tessel-keyring-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("got");
        let script = dir.join("helper.sh");
        std::fs::write(&script, format!("#!/bin/sh\n[ \"$1\" = --login ] || exit 3\ncat > '{}'\n[ -n \"$HOME\" ]\n",
                                        out.display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let user = std::env::var("USER").unwrap();
        let helper = CString::new(script.to_str().unwrap()).unwrap();
        assert!(run_helper(&helper, "--login", user.as_bytes(), b"pass word"));
        assert_eq!(std::fs::read(&out).unwrap(), b"pass word");
        assert!(!run_helper(&helper, "--other", user.as_bytes(), b"x"));
        assert!(!run_helper(&helper, "--login", b"no-such-user-xyz", b"x"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn helper_option_overrides_the_default() {
        let arg = CString::new("helper=/opt/x").unwrap();
        let argv = [arg.as_ptr()];
        assert_eq!(helper_path(1, argv.as_ptr()).to_str().unwrap(), "/opt/x");
        assert_eq!(helper_path(0, ptr::null()).to_str().unwrap(), DEFAULT_HELPER);
    }
}
