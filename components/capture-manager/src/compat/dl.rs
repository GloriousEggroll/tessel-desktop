//! Minimal dlopen wrapper. Handles are never closed (like ctypes.CDLL).

use std::ffi::{c_void, CStr, CString};

#[derive(Clone, Copy, Debug)]
pub struct Library(*mut c_void);

// SAFETY: a dlopen handle is process-global and thread-safe to use with dlsym.
unsafe impl Send for Library {}
unsafe impl Sync for Library {}

fn last_error() -> String {
    let error = unsafe { libc::dlerror() };
    if error.is_null() {
        "unknown dlopen error".into()
    } else {
        unsafe { CStr::from_ptr(error) }.to_string_lossy().into_owned()
    }
}

impl Library {
    /// `ctypes.CDLL(name)`; the error text is dlerror()'s, which is what Python reports.
    pub fn open(name: &str) -> Result<Self, String> {
        let name = CString::new(name).map_err(|_| "embedded null byte".to_string())?;
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            Err(last_error())
        } else {
            Ok(Self(handle))
        }
    }

    /// # Safety
    /// `T` must be the exact function pointer type of the symbol.
    pub unsafe fn symbol<T: Copy>(&self, name: &str) -> Result<T, String> {
        let symbol = CString::new(name).map_err(|_| "embedded null byte".to_string())?;
        let address = libc::dlsym(self.0, symbol.as_ptr());
        if address.is_null() {
            return Err(last_error());
        }
        assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
        Ok(std::mem::transmute_copy(&address))
    }
}
