//! The packaged labwc defaults make Pictures the default for every image type its
//! desktop entry declares, so a stale or third-party handler is never picked first.

use std::collections::BTreeSet;

#[test]
fn every_declared_image_type_defaults_to_pictures() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let desktop = std::fs::read_to_string(root.join("data/org.tessel.Pictures.desktop")).unwrap();
    let declared: BTreeSet<&str> = desktop.lines().find_map(|l| l.strip_prefix("MimeType=")).unwrap()
        .split(';').filter(|m| m.starts_with("image/")).collect();
    let list = std::fs::read_to_string(root.join("../../packaging/defaults/usr/share/applications/labwc-mimeapps.list")).unwrap();
    let defaults: BTreeSet<&str> = list.lines().filter_map(|l| l.split_once('='))
        .filter(|(_, apps)| apps.split(';').next() == Some("org.tessel.Pictures.desktop")).map(|(m, _)| m).collect();
    assert_eq!(declared, defaults);
}
