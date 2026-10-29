//! `file_keys.py`: keyboard-layout independence — resolve keys by their
//! physical (US QWERTY) position. GDK itself is not used here: the lead's
//! `latin_keyval(display, keyval, keycode)` is
//! `latin_keyval_with(keyval, keycode, gdk::keyval_to_lower, display.map_keycode(keycode).map(|e| keyvals))`.

/// XKB keycodes (evdev + 8) of the US QWERTY letters, digits and shortcut
/// punctuation, by row: `(first keycode, characters)`.
pub const ROWS: [(u32, &str); 4] = [(10, "1234567890-="), (24, "qwertyuiop[]"), (38, "asdfghjkl;'"), (52, "zxcvbnm,./")];

/// `QWERTY.get(keycode)`: the keyval at that physical position. Printable
/// ASCII keyvals equal their character codes (`minus` = 0x2d, …).
pub fn qwerty(keycode: u32) -> Option<u32> {
    ROWS.iter().find_map(|(first, chars)| {
        let offset = keycode.checked_sub(*first)? as usize;
        chars.as_bytes().get(offset).map(|&c| c as u32)
    })
}

/// The whole `QWERTY` table as `(keycode, keyval)` pairs.
pub fn qwerty_table() -> Vec<(u32, u32)> {
    ROWS.iter().flat_map(|(first, chars)| chars.bytes().enumerate().map(move |(i, c)| (first + i as u32, c as u32))).collect()
}

fn latin(keyval: u32) -> bool {
    0x20 < keyval && keyval < 0x7f
}

/// `latin_keyval(display, keyval, keycode)`: the ASCII keyval for this key —
/// as typed, from another installed layout (`mapped` = the keyvals
/// `display.map_keycode(keycode)` found, `None` without a display/match),
/// or by position.
pub fn latin_keyval_with(keyval: u32, keycode: u32, to_lower: impl Fn(u32) -> u32, mapped: Option<&[u32]>) -> u32 {
    let lower = to_lower(keyval);
    if latin(lower) {
        return lower;
    }
    if let Some(keyvals) = mapped {
        for &candidate in keyvals {
            if latin(candidate) && candidate == to_lower(candidate) {
                return candidate;
            }
        }
    }
    qwerty(keycode).unwrap_or(lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ascii_lower(keyval: u32) -> u32 {
        if (0x41..=0x5a).contains(&keyval) {
            keyval + 0x20
        } else {
            keyval
        }
    }

    // test_files_browsing.test_vim_keys_and_shortcuts_by_key_position (model part)
    #[test]
    fn keys_by_position() {
        const CYRILLIC_O: u32 = 0x6cf; // The "j" key on a Russian layout.
        const CYRILLIC_ES: u32 = 0x6d3; // The "c" key.
        assert_eq!(latin_keyval_with(CYRILLIC_O, 44, ascii_lower, None), 'j' as u32);
        assert_eq!(latin_keyval_with('C' as u32, 54, ascii_lower, None), 'c' as u32);
        assert_eq!(latin_keyval_with(CYRILLIC_ES, 54, ascii_lower, None), 'c' as u32);
        // Another installed layout wins over the position (only lowercase latin candidates).
        assert_eq!(latin_keyval_with(CYRILLIC_O, 44, ascii_lower, Some(&[CYRILLIC_O, 'J' as u32, 'y' as u32])), 'y' as u32);
        // Unknown keycodes keep the lowered keyval.
        assert_eq!(latin_keyval_with(0xff0d, 36, ascii_lower, None), 0xff0d);
        assert_eq!(qwerty(10), Some('1' as u32));
        assert_eq!(qwerty(21), Some('=' as u32));
        assert_eq!(qwerty(22), None);
        assert_eq!(qwerty(61), Some('/' as u32));
        assert_eq!(qwerty(48), Some('\'' as u32));
        assert_eq!(qwerty(49), None);
        assert_eq!(qwerty_table().len(), 12 + 12 + 11 + 10);
    }
}
