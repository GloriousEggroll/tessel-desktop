//! Pillow `Image.Exif` semantics on top of kamadak-exif: IFD0 + Exif/GPS/Interop sub-IFDs
//! of the primary image, `str(value)` like Pillow (latin-1 strings, rationals as floats,
//! tuples), ASCII writes with `encode("ascii", "replace")`, little-endian `tobytes()`.

use exif::{Context, Field, In, Rational, Tag, Value};
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct ExifData {
    fields: Vec<Field>,
}

impl ExifData {
    /// Raw TIFF-structured EXIF (as stored in JPEG APP1 / PNG eXIf / WebP EXIF).
    pub fn parse(bytes: Vec<u8>) -> Self {
        let mut reader = exif::Reader::new();
        reader.continue_on_error(true);
        let exif = match reader.read_raw(bytes) {
            Ok(exif) => exif,
            Err(exif::Error::PartialResult(partial)) => partial.into_inner().0,
            Err(_) => return Self::default(),
        };
        Self::from_exif(&exif)
    }

    /// IFD0 of a TIFF file.
    pub fn from_container(path: &Path) -> Self {
        let Ok(file) = std::fs::File::open(path) else { return Self::default() };
        let mut reader = exif::Reader::new();
        reader.continue_on_error(true);
        match reader.read_from_container(&mut std::io::BufReader::new(file)) {
            Ok(exif) => Self::from_exif(&exif),
            Err(exif::Error::PartialResult(partial)) => Self::from_exif(&partial.into_inner().0),
            Err(_) => Self::default(),
        }
    }

    fn from_exif(exif: &exif::Exif) -> Self {
        Self { fields: exif.fields().filter(|f| f.ifd_num == In::PRIMARY).cloned().collect() }
    }

    pub fn is_empty(&self) -> bool { self.fields.is_empty() }

    /// `exif.get(tag)` falling back to `exif.get_ifd(0x8769).get(tag)`.
    pub fn field(&self, number: u16) -> Option<&Field> {
        self.fields.iter().find(|f| f.tag.number() == number && f.tag.context() == Context::Tiff)
            .or_else(|| self.fields.iter().find(|f| f.tag.number() == number && f.tag.context() == Context::Exif))
    }

    pub fn orientation(&self) -> Option<u8> {
        self.field(274).and_then(|f| f.value.get_uint(0)).and_then(|v| u8::try_from(v).ok())
    }

    fn replace(&mut self, field: Field) {
        self.fields.retain(|f| f.tag != field.tag);
        self.fields.push(field);
    }

    pub fn set_orientation(&mut self, value: u16) {
        self.replace(Field { tag: Tag::Orientation, ifd_num: In::PRIMARY, value: Value::Short(vec![value]) });
    }

    /// `exif[tag] = text` for IFD0 ASCII tags (Pillow encodes ASCII with "replace").
    pub fn set_text(&mut self, number: u16, text: &str) {
        let bytes: Vec<u8> = text.chars().map(|c| if c.is_ascii() { c as u8 } else { b'?' }).collect();
        self.replace(Field { tag: Tag(Context::Tiff, number), ifd_num: In::PRIMARY, value: Value::Ascii(vec![bytes]) });
    }

    /// PixelXDimension / PixelYDimension (Exif IFD).
    pub fn set_long(&mut self, number: u16, value: u32) {
        self.replace(Field { tag: Tag(Context::Exif, number), ifd_num: In::PRIMARY, value: Value::Long(vec![value]) });
    }

    /// `str(exif.get(tag))` for text tags (latin-1, like Pillow's `load_string`).
    pub fn text(&self, number: u16) -> Option<String> { self.field(number).map(|f| display(&f.value)) }

    /// `str(value)` as Pillow shows it.
    pub fn display(&self, number: u16) -> Option<String> { self.text(number) }

    /// `Exif.tobytes()` (little endian). Falls back to the essential tags if a
    /// stored field cannot be re-encoded.
    pub fn to_bytes(&self) -> Option<Vec<u8>> {
        let write = |fields: &[&Field]| -> Option<Vec<u8>> {
            let mut writer = exif::experimental::Writer::new();
            for field in fields {
                writer.push_field(field);
            }
            let mut buffer = std::io::Cursor::new(Vec::new());
            writer.write(&mut buffer, true).ok()?;
            Some(buffer.into_inner())
        };
        let all: Vec<&Field> = self.fields.iter().collect();
        write(&all).or_else(|| {
            let essential: Vec<&Field> = self.fields.iter()
                .filter(|f| matches!(f.tag.number(), 270 | 271 | 272 | 274 | 40962 | 40963)).collect();
            write(&essential)
        })
    }
}

fn latin1(bytes: &[u8]) -> String { bytes.iter().map(|b| *b as char).collect() }

fn rational(value: &Rational) -> String {
    if value.denom == 0 { "nan".into() } else { tessel_core::pyfmt::float_repr(value.num as f64 / value.denom as f64) }
}

fn tuple(items: Vec<String>) -> String {
    match items.len() {
        1 => items.into_iter().next().unwrap(),
        _ => format!("({})", items.join(", ")),
    }
}

/// Pillow's Python value for a field, as `str()` shows it.
pub fn display(value: &Value) -> String {
    match value {
        Value::Ascii(parts) => parts.first().map(|p| latin1(p)).unwrap_or_default(),
        Value::Byte(v) => tuple(v.iter().map(u8::to_string).collect()),
        Value::Short(v) => tuple(v.iter().map(u16::to_string).collect()),
        Value::Long(v) => tuple(v.iter().map(u32::to_string).collect()),
        Value::SByte(v) => tuple(v.iter().map(i8::to_string).collect()),
        Value::SShort(v) => tuple(v.iter().map(i16::to_string).collect()),
        Value::SLong(v) => tuple(v.iter().map(i32::to_string).collect()),
        Value::Rational(v) => tuple(v.iter().map(rational).collect()),
        Value::SRational(v) => tuple(v.iter().map(|r| if r.denom == 0 { "nan".into() }
                                                      else { tessel_core::pyfmt::float_repr(r.num as f64 / r.denom as f64) }).collect()),
        Value::Float(v) => tuple(v.iter().map(|f| tessel_core::pyfmt::float_repr(*f as f64)).collect()),
        Value::Double(v) => tuple(v.iter().map(|f| tessel_core::pyfmt::float_repr(*f)).collect()),
        Value::Undefined(bytes, _) => crate::compat::bytes_repr(bytes),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_python_strings() {
        let mut exif = ExifData::default();
        exif.set_orientation(6);
        exif.set_text(271, "Test Camera");
        exif.set_text(270, "Café");
        exif.replace(Field { tag: Tag::ExposureTime, ifd_num: In::PRIMARY, value: Value::Rational(vec![Rational { num: 1, denom: 250 }]) });
        let again = ExifData::parse(exif.to_bytes().unwrap());
        assert_eq!(again.orientation(), Some(6));
        assert_eq!(again.text(271).as_deref(), Some("Test Camera"));
        assert_eq!(again.text(270).as_deref(), Some("Caf?"));
        assert_eq!(again.display(33434).as_deref(), Some("0.004"));
    }
}
