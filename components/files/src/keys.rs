//! file_keys.py: keyboard-layout independence — resolve keys by their
//! physical (QWERTY) position.

use gtk::gdk;
use gtk::prelude::*;
use gtk::glib::translate::IntoGlib;

/// XKB keycodes (evdev + 8) of the US QWERTY letters, digits and shortcut punctuation.
const ROWS: [(u32, &str); 4] = [(10, "1234567890-="), (24, "qwertyuiop[]"), (38, "asdfghjkl;'"), (52, "zxcvbnm,./")];

fn char_name(c: char) -> String {
    match c {
        '-' => "minus", '=' => "equal", '[' => "bracketleft", ']' => "bracketright", ';' => "semicolon",
        '\'' => "apostrophe", ',' => "comma", '.' => "period", '/' => "slash",
        _ => return c.to_string(),
    }
    .to_string()
}

/// `QWERTY[keycode]`.
pub fn qwerty(keycode: u32) -> Option<gdk::Key> {
    ROWS.iter().find_map(|(start, chars)| {
        let offset = keycode.checked_sub(*start)? as usize;
        let c = chars.chars().nth(offset)?;
        gdk::Key::from_name(char_name(c))
    })
}

fn latin(key: gdk::Key) -> Option<gdk::Key> {
    let value = key.into_glib();
    (0x20 < value && value < 0x7f).then_some(key)
}

/// The ASCII keyval for this key: as typed, from another installed layout, or by position.
pub fn latin_keyval(display: Option<&gdk::Display>, keyval: gdk::Key, keycode: u32) -> gdk::Key {
    let lower = keyval.to_lower();
    if latin(lower).is_some() {
        return lower;
    }
    if let Some(display) = display {
        if let Some(entries) = display.map_keycode(keycode) {
            for (_keymap_key, candidate) in entries {
                if latin(candidate).is_some() && candidate == candidate.to_lower() {
                    return candidate;
                }
            }
        }
    }
    qwerty(keycode).unwrap_or(lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_fallback() {
        let cyrillic_o = gdk::Key::from_name("Cyrillic_o").unwrap();
        assert_eq!(latin_keyval(None, cyrillic_o, 44), gdk::Key::j);
        assert_eq!(latin_keyval(None, gdk::Key::C, 54), gdk::Key::c);
        assert_eq!(qwerty(20), Some(gdk::Key::minus));
        assert_eq!(qwerty(61), Some(gdk::Key::slash));
        assert_eq!(qwerty(9), None);
    }
}
