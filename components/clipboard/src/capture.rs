//! What to read from a new selection and what to keep of it.

use std::rc::Rc;

/// KDE's hint (KeePassXC, KWallet and others set it): "secret" copies are
/// never stored by clipboard managers.
pub const PASSWORD_HINT_MIME: &str = "x-kde-passwordManagerHint";

/// One kept selection: every format with its bytes, in the owner's order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Contents {
    pub formats: Vec<(String, Rc<Vec<u8>>)>,
}

impl Contents {
    pub fn get(&self, mime: &str) -> Option<&Rc<Vec<u8>>> {
        self.formats.iter().find(|(m, _)| m == mime).map(|(_, data)| data)
    }

    pub fn is_empty(&self) -> bool {
        self.formats.is_empty()
    }

    pub fn mimes(&self) -> impl Iterator<Item = &str> {
        self.formats.iter().map(|(m, _)| m.as_str())
    }
}

/// X11 selection-protocol targets that are not data (Xwayland may pass them through).
fn is_protocol_target(mime: &str) -> bool {
    matches!(mime, "TARGETS" | "MULTIPLE" | "TIMESTAMP" | "SAVE_TARGETS" | "DELETE" | "INCR")
}

/// The formats to read from an offer, in its order, without duplicates.
/// `None` when the offer is tessel-clipboard's own.
pub fn formats_to_read(offered: &[String]) -> Option<Vec<String>> {
    if offered.iter().any(|m| m == crate::MARKER_MIME) {
        return None;
    }
    let mut wanted: Vec<String> = Vec::new();
    for mime in offered {
        if !mime.is_empty() && !is_protocol_target(mime) && !wanted.contains(mime) {
            wanted.push(mime.clone());
        }
    }
    Some(wanted)
}

/// Read results of one selection while its formats arrive.
#[derive(Debug)]
pub struct Capture {
    order: Vec<String>,
    done: Vec<(String, Vec<u8>)>,
    pending: usize,
    budget: usize,
    used: usize,
}

impl Capture {
    pub fn new(order: Vec<String>, budget: usize) -> Self {
        let pending = order.len();
        Capture { order, done: Vec::new(), pending, budget, used: 0 }
    }

    pub fn pending(&self) -> usize {
        self.pending
    }

    /// Whether a format of `len` bytes still fits beside the formats kept already.
    pub fn fits(&self, len: usize) -> bool {
        self.used + len <= self.budget
    }

    /// A format finished reading.
    pub fn finish(&mut self, mime: &str, data: Vec<u8>) {
        self.pending = self.pending.saturating_sub(1);
        if self.used + data.len() <= self.budget {
            self.used += data.len();
            self.done.push((mime.to_owned(), data));
        }
    }

    /// A format failed (closed early, too big, read error, timeout).
    pub fn fail(&mut self) {
        self.pending = self.pending.saturating_sub(1);
    }

    /// What to keep. `None` for a secret copy (with `skip_secrets`) or when
    /// nothing besides the password hint arrived.
    pub fn into_contents(mut self, skip_secrets: bool) -> Option<Contents> {
        let secret = self.done.iter()
            .any(|(m, data)| m == PASSWORD_HINT_MIME && String::from_utf8_lossy(data).trim() == "secret");
        if secret && skip_secrets {
            return None;
        }
        let mut formats = Vec::new();
        for mime in &self.order {
            if let Some(index) = self.done.iter().position(|(m, _)| m == mime) {
                let (mime, data) = self.done.swap_remove(index);
                formats.push((mime, Rc::new(data)));
            }
        }
        formats.iter().any(|(m, _)| m != PASSWORD_HINT_MIME).then_some(Contents { formats })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn own_offers_are_skipped_and_targets_filtered() {
        assert_eq!(formats_to_read(&strings(&["text/plain", crate::MARKER_MIME])), None);
        assert_eq!(formats_to_read(&strings(&["TARGETS", "text/plain", "", "text/plain", "UTF8_STRING"])),
                   Some(strings(&["text/plain", "UTF8_STRING"])));
    }

    #[test]
    fn contents_keep_the_offer_order() {
        let mut capture = Capture::new(strings(&["text/html", "text/plain"]), 100);
        capture.finish("text/plain", b"hi".to_vec());
        capture.finish("text/html", b"<b>hi</b>".to_vec());
        assert_eq!(capture.pending(), 0);
        let contents = capture.into_contents(true).unwrap();
        assert_eq!(contents.mimes().collect::<Vec<_>>(), ["text/html", "text/plain"]);
        assert_eq!(contents.get("text/plain").unwrap().as_slice(), b"hi");
    }

    #[test]
    fn budget_drops_formats_that_do_not_fit() {
        let mut capture = Capture::new(strings(&["text/plain", "image/png"]), 10);
        capture.finish("text/plain", b"hello".to_vec());
        assert!(capture.fits(5) && !capture.fits(6));
        capture.finish("image/png", vec![0; 20]);
        let contents = capture.into_contents(true).unwrap();
        assert_eq!(contents.mimes().collect::<Vec<_>>(), ["text/plain"]);
    }

    #[test]
    fn secrets_are_not_kept_unless_allowed() {
        let make = || {
            let mut capture = Capture::new(strings(&["text/plain", PASSWORD_HINT_MIME]), 100);
            capture.finish("text/plain", b"hunter2".to_vec());
            capture.finish(PASSWORD_HINT_MIME, b"secret\n".to_vec());
            capture
        };
        assert_eq!(make().into_contents(true), None);
        assert!(make().into_contents(false).is_some());
    }

    #[test]
    fn nothing_kept_when_every_format_failed() {
        let mut capture = Capture::new(strings(&["text/plain"]), 100);
        capture.fail();
        assert_eq!(capture.pending(), 0);
        assert_eq!(capture.into_contents(true), None);
    }
}
