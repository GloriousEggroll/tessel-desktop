//! A minimal GNU gettext `.mo` reader (Python's `gettext.translation`).
use std::collections::HashMap;

pub fn parse(data: &[u8]) -> HashMap<String, String> {
    let mut result = HashMap::new();
    if data.len() < 28 {
        return result;
    }
    let little = data[..4] == [0xde, 0x12, 0x04, 0x95];
    let word = |offset: usize| -> Option<usize> {
        let bytes: [u8; 4] = data.get(offset..offset + 4)?.try_into().ok()?;
        Some(if little { u32::from_le_bytes(bytes) } else { u32::from_be_bytes(bytes) } as usize)
    };
    let (Some(count), Some(originals), Some(translations)) = (word(8), word(12), word(16)) else { return result };
    for index in 0..count {
        let entry = |table: usize| -> Option<&str> {
            let length = word(table + index * 8)?;
            let offset = word(table + index * 8 + 4)?;
            std::str::from_utf8(data.get(offset..offset + length)?).ok()
        };
        if let (Some(original), Some(translated)) = (entry(originals), entry(translations)) {
            // Plural entries: msgid\0msgid_plural → first form.
            let key = original.split('\0').next().unwrap_or("");
            if !key.is_empty() {
                result.insert(key.to_owned(), translated.split('\0').next().unwrap_or("").to_owned());
            }
        }
    }
    result
}
