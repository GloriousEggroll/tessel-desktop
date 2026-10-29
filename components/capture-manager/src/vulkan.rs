//! Enumerate Vulkan Video encoding devices without opening a display or stream
//! (capture_vulkan.py). libvulkan is loaded with dlopen on first use.

use crate::compat::dl::Library;
use std::collections::HashSet;
use std::ffi::{c_char, c_void, CStr};
use suite_core::tr;

#[repr(C)]
struct ApplicationInfo {
    s_type: u32,
    p_next: *const c_void,
    application_name: *const c_char,
    application_version: u32,
    engine_name: *const c_char,
    engine_version: u32,
    api_version: u32,
}

#[repr(C)]
struct InstanceInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    application_info: *const ApplicationInfo,
    enabled_layer_count: u32,
    enabled_layer_names: *const c_void,
    enabled_extension_count: u32,
    enabled_extension_names: *const c_void,
}

#[repr(C)]
struct Extension {
    name: [c_char; 256],
    version: u32,
}

pub fn supports_encoding(extensions: &HashSet<String>, codec: &str) -> bool {
    let suffix = match codec { "h264" => "h264", "h265" => "h265", _ => "av1" };
    ["VK_KHR_video_queue".to_string(), "VK_KHR_video_encode_queue".into(), format!("VK_KHR_video_encode_{suffix}")]
        .iter().all(|name| extensions.contains(name))
}

type CreateInstance = unsafe extern "C" fn(*const InstanceInfo, *const c_void, *mut *mut c_void) -> i32;
type DestroyInstance = unsafe extern "C" fn(*mut c_void, *const c_void);
type EnumerateDevices = unsafe extern "C" fn(*mut c_void, *mut u32, *mut *mut c_void) -> i32;
type EnumerateExtensions = unsafe extern "C" fn(*mut c_void, *const c_char, *mut u32, *mut Extension) -> i32;

/// FFmpeg Vulkan device indices supporting this codec; probes check the rest.
pub fn encoding_devices(codec: &str) -> Result<Vec<u32>, String> {
    let library = Library::open("libvulkan.so.1")?;
    let (create, destroy, devices_of, extensions_of) = unsafe {
        (library.symbol::<CreateInstance>("vkCreateInstance")?, library.symbol::<DestroyInstance>("vkDestroyInstance")?,
         library.symbol::<EnumerateDevices>("vkEnumeratePhysicalDevices")?,
         library.symbol::<EnumerateExtensions>("vkEnumerateDeviceExtensionProperties")?)
    };
    let app = ApplicationInfo { s_type: 0, p_next: std::ptr::null(), application_name: c"waycapture".as_ptr(), application_version: 0,
                                engine_name: std::ptr::null(), engine_version: 0, api_version: (1 << 22) | (3 << 12) };
    let info = InstanceInfo { s_type: 1, p_next: std::ptr::null(), flags: 0, application_info: &app, enabled_layer_count: 0,
                              enabled_layer_names: std::ptr::null(), enabled_extension_count: 0, enabled_extension_names: std::ptr::null() };
    let mut instance: *mut c_void = std::ptr::null_mut();
    let status = unsafe { create(&info, std::ptr::null(), &mut instance) };
    if status != 0 {
        return Err(tr!("Vulkan could not initialize (VkResult {status}).", status = status as i64));
    }
    let result = (|| {
        let mut count = 0u32;
        if unsafe { devices_of(instance, &mut count, std::ptr::null_mut()) } != 0 || count > 128 {
            return Err(tr!("Vulkan could not enumerate GPUs."));
        }
        let mut devices = vec![std::ptr::null_mut(); count as usize];
        if unsafe { devices_of(instance, &mut count, devices.as_mut_ptr()) } != 0 {
            return Err(tr!("The Vulkan device list changed. Try recording again."));
        }
        let mut result = Vec::new();
        for (index, device) in devices.iter().take(count as usize).enumerate() {
            let mut n = 0u32;
            if unsafe { extensions_of(*device, std::ptr::null(), &mut n, std::ptr::null_mut()) } != 0 || n > 8192 {
                continue;
            }
            let mut extensions: Vec<Extension> = (0..n).map(|_| Extension { name: [0; 256], version: 0 }).collect();
            if unsafe { extensions_of(*device, std::ptr::null(), &mut n, extensions.as_mut_ptr()) } != 0 {
                continue;
            }
            let names: HashSet<String> = extensions.iter().take(n as usize)
                .map(|e| unsafe { CStr::from_ptr(e.name.as_ptr()) }.to_string_lossy().into_owned()).collect();
            if supports_encoding(&names, codec) {
                result.push(index as u32);
            }
        }
        Ok(result)
    })();
    unsafe { destroy(instance, std::ptr::null()) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_extensions_are_not_encoding_support() {
        let set = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<HashSet<_>>();
        assert!(!supports_encoding(&set(&["VK_KHR_video_queue", "VK_KHR_video_decode_queue", "VK_KHR_video_decode_h264"]), "h264"));
        assert!(supports_encoding(&set(&["VK_KHR_video_queue", "VK_KHR_video_encode_queue", "VK_KHR_video_encode_av1"]), "av1"));
        assert!(!supports_encoding(&set(&["VK_KHR_video_queue", "VK_KHR_video_encode_queue", "VK_KHR_video_encode_av1"]), "h265"));
    }
}
