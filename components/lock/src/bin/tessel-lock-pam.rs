//! tessel-lock-pam: a single unprivileged PAM conversation (Rust port of
//! tessel-lock/pam_helper.c, byte-compatible wire protocol).
//!
//! Never accepts a username or service from the UI; authenticates only the
//! owner (getuid()) with a fixed policy:
//!   (no args)          -> tessel-lock
//!   --password-only    -> tessel-lock-password    (no pam_fprintd)
//!   --fingerprint      -> tessel-lock-fingerprint (pam_fprintd)
//! Any other argv exits 1 without output.
//!
//! Packet framing (stdout): one type byte (PAM msg style 1..4, 5 = success),
//! a network-order u32 length, then UTF-8 (at most 4096 bytes, no NUL).
//! Responses (stdin): network-order u32 length followed by the secret
//! (no newline, no NUL, at most 4096 bytes).
//!
//! Responses are allocated with libc calloc because PAM releases them with
//! free(). On every conversation failure all response buffers are
//! explicit_bzero'ed before being freed. The secret is read straight into
//! the PAM-owned buffer, so no Rust-side copy of it exists.
#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::{c_char, c_int, c_void, CStr};

const PAM_SUCCESS: c_int = 0;
const PAM_BUF_ERR: c_int = 5;
const PAM_CONV_ERR: c_int = 19;
const PAM_DISALLOW_NULL_AUTHTOK: c_int = 0x0001;
const PAM_REFRESH_CRED: c_int = 0x0010;
const PAM_MAX_NUM_MSG: c_int = 32;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_TEXT_INFO: c_int = 4;
const LIMIT: usize = 4096;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type Conversation = unsafe extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int;

#[repr(C)]
struct PamConv {
    conv: Option<Conversation>,
    appdata_ptr: *mut c_void,
}

// Linked by soname from build.rs (`-l:libpam.so.0`).
extern "C" {
    fn pam_start(service: *const c_char, user: *const c_char, conv: *const PamConv, handle: *mut *mut c_void) -> c_int;
    fn pam_authenticate(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_acct_mgmt(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_setcred(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_end(handle: *mut c_void, status: c_int) -> c_int;
}

/// read()/write() the whole buffer, retrying EINTR; false on EOF or error.
fn transfer(fd: c_int, mut bytes: *mut u8, mut size: usize, writing: bool) -> bool {
    while size > 0 {
        // SAFETY: `bytes` points to at least `size` valid bytes (callers).
        let got = unsafe {
            if writing { libc::write(fd, bytes as *const c_void, size) } else { libc::read(fd, bytes as *mut c_void, size) }
        };
        if got < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        if got <= 0 {
            return false;
        }
        // SAFETY: got <= size.
        bytes = unsafe { bytes.add(got as usize) };
        size -= got as usize;
    }
    true
}

/// `packet(type, message)`: strnlen(message, 4096) bytes of the message.
fn packet(kind: u8, message: &[u8]) -> bool {
    let size = message.iter().take(LIMIT).position(|&b| b == 0).unwrap_or(message.len().min(LIMIT));
    let mut kind = kind;
    let mut length = (size as u32).to_be_bytes();
    transfer(libc::STDOUT_FILENO, &mut kind, 1, true)
        && transfer(libc::STDOUT_FILENO, length.as_mut_ptr(), 4, true)
        && transfer(libc::STDOUT_FILENO, message.as_ptr() as *mut u8, size, true)
}

/// Bytes of a PAM message: like C's strnlen(msg ? msg : "", 4096), never
/// reading past the 4096th byte or the terminating NUL.
unsafe fn message_bytes<'a>(message: *const c_char) -> &'a [u8] {
    if message.is_null() {
        return &[];
    }
    // SAFETY: PAM messages are NUL-terminated C strings; strnlen bounds the scan.
    let size = unsafe { libc::strnlen(message, LIMIT) };
    unsafe { std::slice::from_raw_parts(message as *const u8, size) }
}

unsafe extern "C" fn conversation(count: c_int, messages: *mut *const PamMessage, out: *mut *mut PamResponse,
                                  _data: *mut c_void) -> c_int {
    if !(1..=PAM_MAX_NUM_MSG).contains(&count) || messages.is_null() || out.is_null() {
        return PAM_CONV_ERR;
    }
    let count = count as usize;
    // SAFETY: calloc of `count` zeroed pam_response structs; PAM frees it.
    let responses = unsafe { libc::calloc(count, std::mem::size_of::<PamResponse>()) } as *mut PamResponse;
    if responses.is_null() {
        return PAM_BUF_ERR;
    }
    let mut lengths = [0usize; PAM_MAX_NUM_MSG as usize];
    let ok = (|| {
        for (i, length) in lengths.iter_mut().enumerate().take(count) {
            // SAFETY: Linux-PAM passes an array of `count` message pointers.
            let message = unsafe { *messages.add(i) };
            if message.is_null() {
                return false;
            }
            // SAFETY: valid pam_message from PAM.
            let (style, text) = unsafe { ((*message).msg_style, (*message).msg) };
            if !(PAM_PROMPT_ECHO_OFF..=PAM_TEXT_INFO).contains(&style) {
                return false;
            }
            // SAFETY: NUL-terminated (or bounded by 4096).
            if !packet(style as u8, unsafe { message_bytes(text) }) {
                return false;
            }
            if style == PAM_PROMPT_ECHO_OFF || style == PAM_PROMPT_ECHO_ON {
                let mut size = [0u8; 4];
                if !transfer(libc::STDIN_FILENO, size.as_mut_ptr(), 4, false) {
                    return false;
                }
                let size = u32::from_be_bytes(size) as usize;
                if size > LIMIT {
                    return false;
                }
                *length = size;
                // SAFETY: size + 1 zeroed bytes (NUL terminator); PAM frees it.
                let buffer = unsafe { libc::calloc(size + 1, 1) } as *mut u8;
                // SAFETY: `responses` has `count` entries.
                unsafe { (*responses.add(i)).resp = buffer as *mut c_char };
                if buffer.is_null() || !transfer(libc::STDIN_FILENO, buffer, size, false) {
                    return false;
                }
                // SAFETY: buffer holds `size` initialised bytes.
                if unsafe { std::slice::from_raw_parts(buffer, size) }.contains(&0) {
                    return false;
                }
            }
        }
        true
    })();
    if ok {
        // SAFETY: PAM owns and clears the response storage.
        unsafe { *out = responses };
        return PAM_SUCCESS;
    }
    for (i, length) in lengths.iter().enumerate().take(count) {
        // SAFETY: entries are null or calloc'ed buffers of at least `length` bytes.
        unsafe {
            let resp = (*responses.add(i)).resp;
            if !resp.is_null() {
                libc::explicit_bzero(resp as *mut c_void, *length);
                libc::free(resp as *mut c_void);
            }
        }
    }
    // SAFETY: allocated above with calloc.
    unsafe { libc::free(responses as *mut c_void) };
    PAM_CONV_ERR
}

fn run() -> i32 {
    // C's argc includes argv[0]: argc == 1 selects the default policy, argc == 2
    // the exact options, anything else exits 1 without output. (argc == 0 is
    // rejected like C; Linux >= 5.18 turns an empty argv into argc == 1, "".)
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let Some((_, arguments)) = argv.split_first() else { return 1 };
    let service: &CStr = match arguments {
        [] => c"tessel-lock",
        [one] if one.as_encoded_bytes() == b"--password-only" => c"tessel-lock-password",
        [one] if one.as_encoded_bytes() == b"--fingerprint" => c"tessel-lock-fingerprint",
        _ => return 1,
    };
    // SAFETY: plain syscalls.
    unsafe {
        let core = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        libc::setrlimit(libc::RLIMIT_CORE, &core);
        libc::prctl(libc::PR_SET_DUMPABLE, 0 as libc::c_ulong);
        if libc::getuid() != libc::geteuid() || libc::getgid() != libc::getegid() {
            return 1;
        }
    }
    // SAFETY: getpwuid returns static storage or NULL; only pw_name is used before pam_start copies it.
    let account = unsafe { libc::getpwuid(libc::getuid()) };
    if account.is_null() {
        return 1;
    }
    // SAFETY: non-null passwd entry.
    let name = unsafe { (*account).pw_name };
    if name.is_null() {
        return 1;
    }
    let conv = PamConv { conv: Some(conversation), appdata_ptr: std::ptr::null_mut() };
    let mut handle: *mut c_void = std::ptr::null_mut();
    // SAFETY: valid C strings, conv outlives the handle (pam_end below).
    let mut status = unsafe { pam_start(service.as_ptr(), name, &conv, &mut handle) };
    if status != PAM_SUCCESS {
        return 1;
    }
    // SAFETY: handle from pam_start.
    unsafe {
        status = pam_authenticate(handle, PAM_DISALLOW_NULL_AUTHTOK);
        if status == PAM_SUCCESS {
            status = pam_acct_mgmt(handle, 0);
        }
        if status == PAM_SUCCESS {
            status = pam_setcred(handle, PAM_REFRESH_CRED);
        }
    }
    // SAFETY: handle from pam_start; not used afterwards.
    let ended = unsafe { pam_end(handle, status) };
    if status != PAM_SUCCESS || ended != PAM_SUCCESS {
        return 1;
    }
    if packet(5, b"ok") { 0 } else { 1 }
}

fn main() {
    // The Rust runtime ignores SIGPIPE; restore the C helper's default
    // disposition (a vanished client kills the helper, and PAM modules' own
    // children such as unix_chkpwd do not inherit SIG_IGN).
    // SAFETY: plain signal(2) before any other thread exists.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    std::process::exit(run());
}
