//! Remote control without asking.
//!
//! A RemoteDesktop portal request normally opens the picker, because accepting
//! it lets the requesting app see the screen and use the keyboard and pointer.
//! Someone who reaches this computer from elsewhere (RustDesk and the like)
//! has nobody in front of it to answer, and the picker cannot be shown at all
//! over a locked screen. Global Settings can therefore let the picker answer
//! such requests itself, with a screen chosen in advance.
//!
//! Only remote-control requests are answered this way; plain screen sharing
//! keeps asking. The answer never depends on the saved screen being there:
//! with it gone, the first screen the portal offers is used, so a monitor that
//! was unplugged or renamed does not lock the owner out.

use crate::sources::{CaptureSource, Kind};
pub use tessel_core::remote_control::{config_path, RemoteControl, VERSION};

/// The picker's use of the setting.
pub trait Choose {
    /// The screen to hand to a remote-control request without asking, or
    /// `None` to show the picker: the setting is off, or the portal offers no
    /// screen (a window-only request).
    fn choose(&self, sources: &[CaptureSource]) -> Option<CaptureSource>;
}

impl Choose for RemoteControl {
    fn choose(&self, sources: &[CaptureSource]) -> Option<CaptureSource> {
        if !self.unattended {
            return None;
        }
        let mut monitors = sources.iter().filter(|source| source.kind == Kind::Monitor);
        let first = monitors.clone().next()?;
        let saved = if self.output.is_empty() { None } else { monitors.find(|source| source.subtitle == self.output) };
        Some(saved.unwrap_or(first).clone())
    }
}
