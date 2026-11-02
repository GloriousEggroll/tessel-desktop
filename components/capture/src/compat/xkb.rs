//! libxkbcommon keysym lookup, loaded on demand like Python's ctypes.CDLL('libxkbcommon.so.0').

use super::dl::Library;
use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::OnceLock;

type FromName = unsafe extern "C" fn(*const c_char, c_int) -> u32;
type GetName = unsafe extern "C" fn(u32, *mut c_char, usize) -> c_int;

struct Api {
    from_name: FromName,
    get_name: GetName,
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| {
        let library = Library::open("libxkbcommon.so.0")?;
        unsafe {
            Ok(Api { from_name: library.symbol("xkb_keysym_from_name")?, get_name: library.symbol("xkb_keysym_get_name")? })
        }
    })
    .as_ref()
    .map_err(|e| e.clone())
}

/// `xkb_keysym_from_name(name, XKB_KEYSYM_CASE_INSENSITIVE)`; 0 = NoSymbol.
pub fn keysym_from_name(name: &str) -> Result<u32, String> {
    let api = api()?;
    let Ok(name) = CString::new(name) else { return Ok(0) };
    Ok(unsafe { (api.from_name)(name.as_ptr(), 1) })
}

/// `xkb_keysym_get_name` into a 128-byte buffer.
pub fn keysym_name(sym: u32) -> Result<String, String> {
    let api = api()?;
    let mut buffer = [0 as c_char; 128];
    unsafe { (api.get_name)(sym, buffer.as_mut_ptr(), buffer.len()) };
    Ok(unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_string_lossy().into_owned())
}
