//! Peak levels of one PipeWire input, read by a short-lived GStreamer stream
//! (level_monitor.py). `pipewiresrc ! audioconvert ! level ! fakesink`.
//!
//! The capture stream exists only while a meter is visible. It is tagged with
//! `audio.manager.meter` so the audio views hide it, never reconnects to another
//! device, and never changes defaults or volumes. State changes run on one
//! worker thread so a slow PipeWire connection cannot block the GTK main loop;
//! level messages arrive on the main context through the pipeline bus.
//! libgstreamer is `dlopen`ed on first use.

use crate::api::{LevelCallback, LevelMeter};
use glib::ffi::{gboolean, gpointer, GError, GType};
use glib::gobject_ffi::{GObject, GValue, GValueArray};
use glib::prelude::*;
use glib::translate::{from_glib, from_glib_full, from_glib_none, IntoGlib, ToGlibPtr, ToGlibPtrMut};
use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::rc::Rc;
use std::sync::{mpsc, Mutex, OnceLock};

pub const STREAM_PROPERTIES: &str = "props, audio.manager.meter=(string)true, node.dont-reconnect=(string)true, \
node.description=\"Audio Settings level meter\", media.role=Production";

/// Map per-channel dBFS peaks to a 0..1 meter on a -60 dB scale.
pub fn peak_level(peaks: &[f64]) -> f64 {
    let loudest = peaks.iter().copied().filter(|v| v.is_finite()).reduce(f64::max).unwrap_or(f64::NEG_INFINITY);
    if !loudest.is_finite() {
        return 0.0;
    }
    ((loudest + 60.0) / 60.0).clamp(0.0, 1.0)
}

type Ptr = *mut c_void;

#[allow(non_snake_case)]
struct Gst {
    gst_init_check: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char, *mut *mut GError) -> gboolean,
    gst_element_factory_find: unsafe extern "C" fn(*const c_char) -> Ptr,
    gst_object_unref: unsafe extern "C" fn(Ptr),
    gst_element_factory_make: unsafe extern "C" fn(*const c_char, *const c_char) -> *mut GObject,
    gst_pipeline_new: unsafe extern "C" fn(*const c_char) -> *mut GObject,
    gst_bin_add: unsafe extern "C" fn(*mut GObject, *mut GObject) -> gboolean,
    gst_element_link: unsafe extern "C" fn(*mut GObject, *mut GObject) -> gboolean,
    gst_structure_from_string: unsafe extern "C" fn(*const c_char, *mut *mut c_char) -> Ptr,
    gst_structure_get_type: unsafe extern "C" fn() -> GType,
    gst_structure_get_name: unsafe extern "C" fn(Ptr) -> *const c_char,
    gst_structure_get_value: unsafe extern "C" fn(Ptr, *const c_char) -> *const GValue,
    gst_pipeline_get_bus: unsafe extern "C" fn(*mut GObject) -> *mut GObject,
    gst_bus_add_signal_watch: unsafe extern "C" fn(*mut GObject),
    gst_bus_remove_signal_watch: unsafe extern "C" fn(*mut GObject),
    gst_element_set_state: unsafe extern "C" fn(*mut GObject, c_int) -> c_int,
    gst_message_get_structure: unsafe extern "C" fn(Ptr) -> Ptr,
}

unsafe impl Send for Gst {}
unsafe impl Sync for Gst {}

fn gst() -> Option<&'static Gst> {
    static GST: OnceLock<Option<Gst>> = OnceLock::new();
    GST.get_or_init(|| unsafe {
        let handle = libc::dlopen(c"libgstreamer-1.0.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
        if handle.is_null() {
            return None;
        }
        macro_rules! sym {
            ($name:ident) => {{
                let symbol = libc::dlsym(handle, concat!(stringify!($name), "\0").as_ptr().cast());
                if symbol.is_null() {
                    return None;
                }
                std::mem::transmute(symbol)
            }};
        }
        let api = Gst {
            gst_init_check: sym!(gst_init_check),
            gst_element_factory_find: sym!(gst_element_factory_find),
            gst_object_unref: sym!(gst_object_unref),
            gst_element_factory_make: sym!(gst_element_factory_make),
            gst_pipeline_new: sym!(gst_pipeline_new),
            gst_bin_add: sym!(gst_bin_add),
            gst_element_link: sym!(gst_element_link),
            gst_structure_from_string: sym!(gst_structure_from_string),
            gst_structure_get_type: sym!(gst_structure_get_type),
            gst_structure_get_name: sym!(gst_structure_get_name),
            gst_structure_get_value: sym!(gst_structure_get_value),
            gst_pipeline_get_bus: sym!(gst_pipeline_get_bus),
            gst_bus_add_signal_watch: sym!(gst_bus_add_signal_watch),
            gst_bus_remove_signal_watch: sym!(gst_bus_remove_signal_watch),
            gst_element_set_state: sym!(gst_element_set_state),
            gst_message_get_structure: sym!(gst_message_get_structure),
        };
        if (api.gst_init_check)(std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
            return None;
        }
        Some(api)
    })
    .as_ref()
}

const ELEMENTS: [&CStr; 4] = [c"pipewiresrc", c"audioconvert", c"level", c"fakesink"];

/// `available()`: GStreamer and the four elements exist.
pub fn available() -> bool {
    let Some(api) = gst() else { return false };
    ELEMENTS.iter().all(|name| unsafe {
        let factory = (api.gst_element_factory_find)(name.as_ptr());
        if factory.is_null() {
            false
        } else {
            (api.gst_object_unref)(factory);
            true
        }
    })
}

struct SendPtr(*mut GObject);
unsafe impl Send for SendPtr {}

/// One worker thread performs every state change, in order.
fn submit(pipeline: &glib::Object, state: c_int) {
    static WORKER: OnceLock<Mutex<mpsc::Sender<(SendPtr, c_int)>>> = OnceLock::new();
    let sender = WORKER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<(SendPtr, c_int)>();
        std::thread::Builder::new().name("audio-level".into()).spawn(move || {
            let api = gst().expect("GStreamer loaded before submitting");
            for (pipeline, state) in receiver {
                unsafe {
                    (api.gst_element_set_state)(pipeline.0, state);
                    glib::gobject_ffi::g_object_unref(pipeline.0.cast());
                }
            }
        }).expect("level worker thread");
        Mutex::new(sender)
    });
    let pointer = unsafe { glib::gobject_ffi::g_object_ref(pipeline.as_ptr().cast()) } as *mut GObject;
    let _ = sender.lock().unwrap().send((SendPtr(pointer), state));
}

const STATE_NULL: c_int = 1;
const STATE_PLAYING: c_int = 4;

struct Inner {
    closed: Cell<bool>,
    pipeline: RefCell<Option<glib::Object>>,
    bus: glib::Object,
    handlers: RefCell<Vec<glib::SignalHandlerId>>,
}

pub struct PeakMonitor(Rc<Inner>);

unsafe fn peaks(api: &Gst, message: Ptr) -> Option<Vec<f64>> {
    let structure = (api.gst_message_get_structure)(message);
    if structure.is_null() || CStr::from_ptr((api.gst_structure_get_name)(structure)).to_bytes() != b"level" {
        return None;
    }
    let value = (api.gst_structure_get_value)(structure, c"peak".as_ptr());
    if value.is_null() {
        return Some(Vec::new());
    }
    let value: glib::Value = from_glib_none(value);
    if value.type_().into_glib() != glib::gobject_ffi::g_value_array_get_type() {
        return Some(Vec::new());
    }
    let array = glib::gobject_ffi::g_value_get_boxed(value.to_glib_none().0) as *const GValueArray;
    if array.is_null() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for i in 0..(*array).n_values as usize {
        let item: glib::Value = from_glib_none((*array).values.add(i) as *const GValue);
        if let Ok(v) = item.get::<f64>() {
            out.push(v);
                }
    }
    Some(out)
}

impl PeakMonitor {
    pub fn start(target: i64, callback: LevelCallback) -> Result<Self, String> {
        Self::with_interval(target, callback, 60)
    }

    pub fn with_interval(target: i64, callback: LevelCallback, interval_ms: u64) -> Result<Self, String> {
        if !available() {
            return Err("GStreamer PipeWire level metering is unavailable.".into());
        }
        let api = gst().unwrap();
        unsafe {
            let pipeline: glib::Object = from_glib_none((api.gst_pipeline_new)(c"audio-manager-level-meter".as_ptr()));
            let make = |name: &CStr| -> Result<glib::Object, String> {
                let element = (api.gst_element_factory_make)(name.as_ptr(), std::ptr::null());
                if element.is_null() {
                    return Err("GStreamer PipeWire level metering is unavailable.".into());
                }
                Ok(from_glib_none(element))
            };
            let source = make(c"pipewiresrc")?;
            source.set_property("target-object", target.to_string());
            source.set_property("client-name", "Audio Settings level meter");
            let text = CString::new(STREAM_PROPERTIES).unwrap();
            let structure = (api.gst_structure_from_string)(text.as_ptr(), std::ptr::null_mut());
            if !structure.is_null() {
                let structure_type: glib::Type = from_glib((api.gst_structure_get_type)());
                let mut value = glib::Value::from_type(structure_type);
                glib::gobject_ffi::g_value_take_boxed(value.to_glib_none_mut().0, structure);
                source.set_property_from_value("stream-properties", &value);
            }
            let convert = make(c"audioconvert")?;
            let level = make(c"level")?;
            level.set_property("interval", interval_ms * 1_000_000);
            level.set_property("post-messages", true);
            let sink = make(c"fakesink")?;
            sink.set_property("sync", false);
            sink.set_property("async", false);
            for element in [&source, &convert, &level, &sink] {
                (api.gst_bin_add)(pipeline.as_ptr(), element.as_ptr());
            }
            (api.gst_element_link)(source.as_ptr(), convert.as_ptr());
            (api.gst_element_link)(convert.as_ptr(), level.as_ptr());
            (api.gst_element_link)(level.as_ptr(), sink.as_ptr());
            let bus: glib::Object = from_glib_full((api.gst_pipeline_get_bus)(pipeline.as_ptr()));
            (api.gst_bus_add_signal_watch)(bus.as_ptr());
            let inner = Rc::new(Inner { closed: Cell::new(false), pipeline: RefCell::new(Some(pipeline.clone())), bus: bus.clone(), handlers: RefCell::default() });
            let callback = Rc::new(callback);
            let (weak, cb) = (Rc::downgrade(&inner), callback.clone());
            let element = bus.connect_local("message::element", false, move |args| {
                let inner = weak.upgrade()?;
                if inner.closed.get() {
                    return None;
                }
                let message = glib::gobject_ffi::g_value_get_boxed(args[1].to_glib_none().0) as gpointer;
                if let Some(peaks) = peaks(api, message) {
                    cb(Some(peak_level(&peaks)));
                }
                None
            });
            let (weak, cb) = (Rc::downgrade(&inner), callback);
            let error = bus.connect_local("message::error", false, move |_| {
                let inner = weak.upgrade()?;
                if inner.closed.get() {
                    return None;
                }
                // The device vanished or refused capture: show silence, stop quietly.
                cb(None);
                PeakMonitor(inner).stop();
                None
            });
            inner.handlers.borrow_mut().extend([element, error]);
            submit(&pipeline, STATE_PLAYING);
            Ok(PeakMonitor(inner))
        }
    }

    pub fn stop(&self) {
        if self.0.closed.replace(true) {
            return;
        }
        for handler in self.0.handlers.borrow_mut().drain(..) {
            self.0.bus.disconnect(handler);
        }
        if let Some(api) = gst() {
            unsafe { (api.gst_bus_remove_signal_watch)(self.0.bus.as_ptr()) };
        }
        if let Some(pipeline) = self.0.pipeline.borrow_mut().take() {
            submit(&pipeline, STATE_NULL);
        }
    }
}

impl LevelMeter for PeakMonitor {
    fn stop(&self) {
        PeakMonitor::stop(self)
    }
}

impl Drop for PeakMonitor {
    fn drop(&mut self) {
        if Rc::strong_count(&self.0) == 1 {
            self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_levels_use_loudest_channel_on_60db_scale() {
        assert_eq!(peak_level(&[-700.0, f64::NEG_INFINITY]), 0.0);
        assert!((peak_level(&[-30.0, -6.0]) - 0.9).abs() < 1e-9);
        assert_eq!(peak_level(&[3.0]), 1.0);
        assert_eq!(peak_level(&[]), 0.0);
        assert_eq!(peak_level(&[f64::NAN]), 0.0);
    }
}
