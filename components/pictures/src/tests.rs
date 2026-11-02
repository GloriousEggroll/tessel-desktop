//! Port of test_picture_model.py (PictureTests, DesktopEntryTests) plus pixel-exact
//! checks of the Pillow formulas (references generated with Pillow 12.3).

use crate::exif_data::ExifData;
use crate::model::*;
use image::{Rgba, RgbaImage};
use std::fs;
use std::path::{Path, PathBuf};

const ADJUST: &[u8] = &[177, 52, 52, 255, 50, 78, 99, 160, 186, 78, 86, 255, 107, 223, 223, 0, 170, 170, 170, 77, 159, 223, 112, 255, 255, 255, 255, 255, 0, 0, 0, 255, 179, 98, 105, 200, 85, 86, 86, 63, 66, 89, 110, 255, 210, 150, 99, 128, 111, 118, 192, 255, 0, 0, 0, 5, 176, 53, 52, 255];
const ADJUST2: &[u8] = &[255, 0, 0, 255, 0, 8, 59, 160, 255, 0, 0, 255, 0, 255, 255, 0, 55, 55, 55, 77, 0, 252, 0, 255, 230, 230, 230, 255, 0, 0, 0, 255, 255, 0, 0, 200, 6, 6, 8, 63, 0, 12, 73, 255, 255, 5, 0, 128, 0, 6, 255, 255, 0, 0, 0, 5, 255, 0, 0, 255];
const GRAY: &[u8] = &[76, 76, 76, 255, 51, 51, 51, 160, 82, 82, 82, 255, 172, 172, 172, 0, 128, 128, 128, 77, 146, 146, 146, 255, 250, 250, 250, 255, 0, 0, 0, 255, 90, 90, 90, 200, 61, 61, 61, 63, 60, 60, 60, 255, 133, 133, 133, 128, 96, 96, 96, 255, 5, 5, 5, 5, 66, 66, 66, 255];
const AUTO: &[u8] = &[255, 0, 0, 255, 20, 61, 91, 160, 200, 30, 40, 255, 10, 244, 255, 0, 128, 130, 130, 77, 90, 204, 15, 255, 250, 255, 255, 255, 0, 0, 0, 255, 180, 51, 61, 200, 60, 62, 63, 63, 33, 67, 100, 255, 240, 102, 20, 128, 70, 81, 255, 255, 5, 5, 5, 5, 199, 10, 9, 255];
const WHITE: &[u8] = &[255, 0, 0, 108, 133, 151, 200, 30, 40, 255, 255, 255, 217, 217, 217, 90, 200, 15, 250, 250, 250, 0, 0, 0, 196, 94, 102, 207, 207, 207, 33, 66, 99, 247, 177, 137, 70, 80, 250, 250, 250, 250, 199, 10, 9];
const REDEYE: &[u8] = &[0, 0, 0, 255, 20, 60, 90, 160, 35, 30, 40, 255, 10, 240, 250, 0, 128, 128, 128, 77, 90, 200, 15, 255, 250, 250, 250, 255, 0, 0, 0, 255, 55, 50, 60, 200, 60, 61, 62, 63, 33, 66, 99, 255, 60, 100, 20, 128, 70, 80, 250, 255, 5, 5, 5, 5, 9, 10, 9, 255];
const LEFT: &[u8] = &[128, 128, 128, 77, 60, 61, 62, 63, 199, 10, 9, 255, 10, 240, 250, 0, 180, 50, 60, 200, 5, 5, 5, 5, 200, 30, 40, 255, 0, 0, 0, 255, 70, 80, 250, 255, 20, 60, 90, 160, 250, 250, 250, 255, 240, 100, 20, 128, 255, 0, 0, 255, 90, 200, 15, 255, 33, 66, 99, 255];
const RIGHT: &[u8] = &[33, 66, 99, 255, 90, 200, 15, 255, 255, 0, 0, 255, 240, 100, 20, 128, 250, 250, 250, 255, 20, 60, 90, 160, 70, 80, 250, 255, 0, 0, 0, 255, 200, 30, 40, 255, 5, 5, 5, 5, 180, 50, 60, 200, 10, 240, 250, 0, 199, 10, 9, 255, 60, 61, 62, 63, 128, 128, 128, 77];
const COMPOSITE: &[u8] = &[10, 200, 30, 180, 115, 122, 61, 206, 115, 122, 61, 206, 10, 200, 30, 180, 115, 122, 61, 206, 1, 2, 3, 255];

const FIXTURE: [[u8; 4]; 15] = [[255, 0, 0, 255], [20, 60, 90, 160], [200, 30, 40, 255], [10, 240, 250, 0], [128, 128, 128, 77],
    [90, 200, 15, 255], [250, 250, 250, 255], [0, 0, 0, 255], [180, 50, 60, 200], [60, 61, 62, 63],
    [33, 66, 99, 255], [240, 100, 20, 128], [70, 80, 250, 255], [5, 5, 5, 5], [199, 10, 9, 255]];

fn fixture() -> RgbaImage {
    RgbaImage::from_fn(5, 3, |x, y| Rgba(FIXTURE[(y * 5 + x) as usize]))
}

#[test]
fn pixel_operations_match_pillow() {
    let image = fixture();
    assert_eq!(adjusted(&image, 1.2, 1.1, 0.5, 1.2).as_raw().as_slice(), ADJUST);
    assert_eq!(adjusted(&image, 0.7, 1.6, 2.2, 0.6).as_raw().as_slice(), ADJUST2);
    assert_eq!(autocontrast(&image).as_raw().as_slice(), AUTO);
    assert_eq!(over_white(&image).as_raw().as_slice(), WHITE);
    assert_eq!(image::imageops::rotate270(&image).as_raw().as_slice(), LEFT);
    assert_eq!(image::imageops::rotate90(&image).as_raw().as_slice(), RIGHT);
    let mut base = RgbaImage::from_pixel(3, 2, Rgba([10, 200, 30, 180]));
    let mut overlay = RgbaImage::from_pixel(2, 2, Rgba([250, 20, 100, 90]));
    overlay.put_pixel(1, 1, Rgba([1, 2, 3, 255]));
    alpha_composite(&mut base, &overlay, 1, 0);
    assert_eq!(base.as_raw().as_slice(), COMPOSITE);
}

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn temp(name: &str) -> Dir {
    let path = std::env::temp_dir().join(format!("pictures-model-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    Dir(path)
}

/// setUp: 24×16 RGBA (20, 60, 90, 160) with a red (0, 0) pixel, saved as PNG.
fn original(dir: &Dir) -> PathBuf {
    let path = dir.0.join("original.png");
    let mut image = RgbaImage::from_pixel(24, 16, Rgba([20, 60, 90, 160]));
    image.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
    image.save(&path).unwrap();
    path
}

fn temporaries(folder: &Path) -> usize {
    fs::read_dir(folder).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with(".tessel-pictures-")).count()
}

fn icc_of(path: &Path) -> Option<Vec<u8>> {
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::open(path).unwrap().with_guessed_format().unwrap().into_decoder().unwrap();
    decoder.icc_profile().unwrap()
}

fn exif_of(path: &Path) -> ExifData {
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::open(path).unwrap().with_guessed_format().unwrap().into_decoder().unwrap();
    ExifData::parse(decoder.exif_metadata().unwrap().unwrap())
}

#[test]
fn edit_history_and_original_untouched_until_save() {
    let dir = temp("history");
    let path = original(&dir);
    let before = fs::read(&path).unwrap();
    let mut doc = PictureDocument::open(&path).unwrap();
    doc.apply(Operation::Right).unwrap();
    assert_eq!(doc.image.dimensions(), (16, 24));
    doc.apply(Operation::Crop([1.0, 2.0, 10.0, 8.0])).unwrap();
    assert_eq!(doc.image.dimensions(), (10, 8));
    doc.apply(Operation::Resize(5.0, 4.0)).unwrap();
    assert_eq!(doc.image.dimensions(), (5, 4));
    assert!(doc.dirty());
    assert_eq!(fs::read(&path).unwrap(), before);
    doc.undo(false).unwrap();
    assert_eq!(doc.image.dimensions(), (10, 8));
    doc.undo(true).unwrap();
    assert_eq!(doc.image.dimensions(), (5, 4));
    doc.save(None, false, 95, None).unwrap();
    assert!(!doc.dirty());
    assert_eq!(image::image_dimensions(&path).unwrap(), (5, 4));
    assert!(icc_of(&path).is_some());
    doc.undo(false).unwrap();
    assert!(doc.dirty());
    doc.undo(true).unwrap();
    assert!(!doc.dirty());
}

#[test]
fn metadata_orientation_and_atomic_format_conversion() {
    let dir = temp("orientation");
    let jpeg = dir.0.join("oriented.jpg");
    let mut exif = ExifData::default();
    exif.set_orientation(6);
    exif.set_text(271, "Test Camera");
    exif.set_text(270, "Original description");
    {
        use image::ImageEncoder;
        let mut file = fs::File::create(&jpeg).unwrap();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, 90);
        encoder.set_exif_metadata(exif.to_bytes().unwrap()).unwrap();
        let red = image::RgbImage::from_pixel(30, 20, image::Rgb([255, 0, 0]));
        encoder.write_image(red.as_raw(), 30, 20, image::ExtendedColorType::Rgb8).unwrap();
    }
    let mut doc = PictureDocument::open(&jpeg).unwrap();
    assert_eq!(doc.image.dimensions(), (20, 30));
    assert_eq!(doc.comment, "Original description");
    doc.apply(Operation::Comment("Edited description".into())).unwrap();
    let png = dir.0.join("export.png");
    doc.save(Some(&png), false, 95, None).unwrap();
    let saved = exif_of(&png);
    assert_eq!(saved.orientation(), Some(1));
    assert_eq!(saved.text(271).as_deref(), Some("Test Camera"));
    assert_eq!(saved.text(270).as_deref(), Some("Edited description"));
    assert_eq!(temporaries(&dir.0), 0);
}

#[test]
fn external_changes_and_existing_destinations_are_not_overwritten() {
    let dir = temp("external");
    let path = original(&dir);
    let mut doc = PictureDocument::open(&path).unwrap();
    doc.apply(Operation::Mirror).unwrap();
    let other = dir.0.join("other.png");
    fs::write(&other, b"existing file").unwrap();
    assert!(matches!(doc.save(Some(&other), false, 95, None), Err(PictureError::FileExists(_))));
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&path, b"external change").unwrap();
    match doc.save(None, false, 95, None) {
        Err(PictureError::Runtime(message)) => assert!(message.contains("changed outside")),
        other => panic!("{other:?}"),
    }
    assert_eq!(fs::read(&other).unwrap(), b"existing file");
    assert_eq!(fs::read(&path).unwrap(), b"external change");
}

#[test]
fn failed_save_leaves_original_and_history_intact() {
    let dir = temp("failed");
    let path = original(&dir);
    let mut doc = PictureDocument::open(&path).unwrap();
    doc.apply(Operation::Flip).unwrap();
    let before = fs::read(&path).unwrap();
    doc.encoder = |_, _, _, _, _, _| Err("disk full".into());
    assert_eq!(doc.save(None, false, 95, None).unwrap_err(), PictureError::Os("disk full".into()));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(doc.dirty());
    assert_eq!(temporaries(&dir.0), 0);
}

#[test]
fn new_save_on_filesystem_without_hardlinks() {
    let dir = temp("nolinks");
    let path = original(&dir);
    let mut doc = PictureDocument::open(&path).unwrap();
    doc.link = |_, _| Err(std::io::Error::from_raw_os_error(libc::EOPNOTSUPP));
    let target = dir.0.join("usb-copy.png");
    doc.save(Some(&target), false, 95, None).unwrap();
    assert_eq!(image::image_dimensions(&target).unwrap(), doc.image.dimensions());
    assert_eq!(temporaries(&dir.0), 0);
    // A destination that appeared meanwhile is never replaced (RENAME_NOREPLACE).
    let mut doc = PictureDocument::open(&path).unwrap();
    doc.link = |_, _| Err(std::io::Error::from_raw_os_error(libc::EXDEV));
    let raced = dir.0.join("raced.png");
    doc.encoder = |image, format, path, quality, exif, icc| {
        fs::write(path.parent().unwrap().join("raced.png"), b"other").unwrap();
        encode(image, format, path, quality, exif, icc)
    };
    assert!(matches!(doc.save(Some(&raced), false, 95, None), Err(PictureError::Os(m)) if m.contains("File exists")));
    assert_eq!(fs::read(&raced).unwrap(), b"other");
    assert_eq!(temporaries(&dir.0), 0);
}

#[test]
fn red_eye_annotations_and_adjustments_are_undoable() {
    let dir = temp("redeye");
    let path = original(&dir);
    let mut doc = PictureDocument::open(&path).unwrap();
    let original = doc.image.clone();
    doc.apply(Operation::RedEye([0.0, 0.0, 2.0, 2.0])).unwrap();
    assert_eq!(doc.image.get_pixel(0, 0), &Rgba([0, 0, 0, 255]));
    assert_eq!(doc.image.get_pixel(10, 10), &Rgba([20, 60, 90, 160]));
    doc.apply(Operation::Annotate(Annotation { kind: Some(AnnotationKind::Arrow), points: vec![(5.0, 5.0), (20.0, 10.0)],
                                               color: [255, 255, 0, 255], width: 2, ..Default::default() })).unwrap();
    assert!(doc.image.pixels().any(|p| p == &Rgba([255, 255, 0, 255])));
    doc.apply(Operation::Adjust { brightness: 1.2, contrast: 1.1, saturation: 0.5, gamma: 1.2 }).unwrap();
    assert_eq!(doc.image.get_pixel(0, 0)[3], 255);
    while doc.can_undo() {
        doc.undo(false).unwrap();
    }
    assert_eq!(doc.image, original);
    assert!(!doc.dirty());
    // The full-region red-eye matches Pillow pixel for pixel.
    let fixture_path = dir.0.join("fixture.png");
    fixture().save(&fixture_path).unwrap();
    let mut doc = PictureDocument::open(&fixture_path).unwrap();
    doc.apply(Operation::RedEye([0.0, 0.0, 5.0, 3.0])).unwrap();
    assert_eq!(doc.image.as_raw().as_slice(), REDEYE);
    doc.apply(Operation::Grayscale).unwrap();
    doc.undo(false).unwrap();
    doc.undo(false).unwrap();
    doc.apply(Operation::Grayscale).unwrap();
    assert_eq!(doc.image.as_raw().as_slice(), GRAY);
    assert!(matches!(doc.apply(Operation::Crop([4.0, 0.0, 2.0, 1.0])), Err(PictureError::Value(_))));
    assert!(matches!(doc.apply(Operation::Annotate(Annotation::default())), Err(PictureError::Value(m)) if m == "Choose an annotation tool."));
}

#[test]
fn annotation_tools_draw_inside_the_picture() {
    let dir = temp("annotate");
    let path = original(&dir);
    let doc = PictureDocument::open(&path).unwrap();
    for kind in [AnnotationKind::Rectangle, AnnotationKind::Ellipse, AnnotationKind::Line, AnnotationKind::Pen, AnnotationKind::Text] {
        let result = doc.annotate(&Annotation { kind: Some(kind.clone()), points: vec![(2.0, 2.0), (6.0, 4.0), (20.0, 12.0)],
                                                color: [0, 255, 0, 255], width: 1, text: "Hi".into(), stamp: None }).unwrap();
        assert!(result.pixels().any(|p| p[1] > 200 && p[0] < 100), "{kind:?}");
        assert_eq!(result.dimensions(), (24, 16));
    }
    let stamp = dir.0.join("stamp.png");
    RgbaImage::from_pixel(40, 40, Rgba([1, 2, 3, 255])).save(&stamp).unwrap();
    let result = doc.annotate(&Annotation { kind: Some(AnnotationKind::Stamp), points: vec![(4.0, 4.0), (12.0, 10.0)],
                                            stamp: Some(stamp), ..Default::default() }).unwrap();
    assert_eq!(result.get_pixel(5, 5), &Rgba([1, 2, 3, 255]));
    assert_eq!(result.get_pixel(20, 14), &Rgba([20, 60, 90, 160]));
}

#[test]
fn history_bounded_and_new_edit_discards_redo() {
    let dir = temp("bounded");
    let path = original(&dir);
    let mut doc = PictureDocument::open(&path).unwrap();
    doc.max_history = 3;
    for _ in 0..6 {
        doc.apply(Operation::Flip).unwrap();
    }
    let pngs = |doc: &PictureDocument| fs::read_dir(&doc.temporary).unwrap().filter(|e| e.as_ref().unwrap().path().extension()
        .is_some_and(|x| x == "png")).count();
    assert_eq!(doc.history.len(), 3);
    assert_eq!(pngs(&doc), 3);
    doc.undo(false).unwrap();
    doc.apply(Operation::Mirror).unwrap();
    assert!(!doc.can_redo());
    assert_eq!(pngs(&doc), 3);
    let temporary = doc.temporary.clone();
    drop(doc);
    assert!(!temporary.exists());
}

#[test]
fn animation_not_silently_overwritten_and_folder_filtering() {
    let dir = temp("animation");
    let gif = dir.0.join("animated.gif");
    {
        let mut file = fs::File::create(&gif).unwrap();
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut file);
        encoder.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
        for color in [[255, 0, 0, 255], [0, 128, 0, 255]] {
            encoder.encode_frame(image::Frame::from_parts(RgbaImage::from_pixel(10, 10, Rgba(color)), 0, 0,
                                                         image::Delay::from_numer_denom_ms(100, 1))).unwrap();
        }
    }
    let before = fs::read(&gif).unwrap();
    let mut doc = PictureDocument::open(&gif).unwrap();
    assert_eq!(doc.frames, 2);
    assert_eq!(doc.metadata.duration, Some(100));
    assert!(matches!(doc.apply(Operation::Right), Err(PictureError::Value(m)) if m.contains("still-frame")));
    assert!(matches!(doc.save(None, false, 95, None), Err(PictureError::Value(m)) if m.contains("Save As")));
    assert_eq!(fs::read(&gif).unwrap(), before);
    doc.save(Some(&dir.0.join("frame.png")), false, 95, Some(1)).unwrap();
    assert_eq!(doc.image.get_pixel(0, 0), &Rgba([0, 128, 0, 255]));
    assert!(!doc.dirty());
    assert!(!doc.can_undo());
    assert_eq!(fs::read(&gif).unwrap(), before);
    fs::write(dir.0.join("notes.txt"), "not an image").unwrap();
    for name in ["image10.png", "image2.png", ".hidden.png"] {
        RgbaImage::new(2, 2).save(dir.0.join(name)).unwrap();
    }
    fs::create_dir(dir.0.join("zfolder")).unwrap();
    let names: Vec<String> = scan_folder(&dir.0, false, "name", false).unwrap().iter()
        .map(|e| e.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert!(!names.contains(&"notes.txt".to_string()));
    assert!(!names.contains(&".hidden.png".to_string()));
    assert_eq!(names[0], "zfolder");
    let index = |n: &str| names.iter().position(|x| x == n).unwrap();
    assert!(index("image2.png") < index("image10.png"));
    assert!(scan_folder(&dir.0, true, "name", false).unwrap().iter().any(|e| e.path.ends_with(".hidden.png")));
    let reversed: Vec<String> = scan_folder(&dir.0, false, "name", true).unwrap().iter()
        .map(|e| e.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(reversed[0], "zfolder");
    assert!(reversed.iter().position(|x| x == "image10.png") < reversed.iter().position(|x| x == "image2.png"));
    assert!(checked_size(100000, 100000).is_err());
    assert!(natural_key("File2.png") < natural_key("file10.png"));
    assert_eq!(natural_key("a01"), natural_key("A1"));
}

#[test]
fn unknown_files_report_the_size_error() {
    let dir = temp("unknown");
    let path = dir.0.join("broken.png.txt");
    fs::write(&path, b"not an image").unwrap();
    assert_eq!(load_pixels(&path, None, 0).err().unwrap().to_string(), "Image size must be positive and at most 80 megapixels.");
    assert!(PictureDocument::open(&dir.0.join("missing.png")).err().unwrap().to_string().starts_with("[Errno 2] No such file or directory"));
}

/// The .desktop MimeType list covers every played video type.
#[test]
fn desktop_file_lists_every_played_video_type() {
    let types: Vec<&str> = crate::files::DESKTOP_MIME_TYPES.split(';').collect();
    for extension in VIDEO_EXTENSIONS {
        let (guess, _) = gio::content_type_guess(Some(format!("clip{extension}")), None::<&[u8]>);
        assert!(types.contains(&guess.as_str()), "{extension} {guess}");
    }
    let desktop = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/org.tessel.Pictures.desktop");
    let text = fs::read_to_string(desktop).unwrap();
    assert!(text.lines().any(|l| l == format!("MimeType={}", crate::files::DESKTOP_MIME_TYPES)));
    assert!(text.contains("StartupWMClass=org.tessel.Pictures"));
}

#[test]
fn cli_matches_argparse() {
    use crate::app::{arguments, check, Exit, Options, HELP};
    let args = |list: &[&str]| arguments(&list.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(args(&["-fs", "a.png", "b.png"]).unwrap(),
               Options { files: vec!["a.png".into(), "b.png".into()], fullscreen: true, slideshow: true, check: false });
    assert!(args(&["--full", "--slide", "--ch"]).unwrap().check);
    assert_eq!(args(&["--", "-x.png"]).unwrap().files, ["-x.png"]);
    assert_eq!(args(&["--help"]).unwrap_err(), Exit { code: 0, stdout: HELP.into(), stderr: String::new() });
    let usage = "usage: picture_viewer.py [-h] [-f] [-s] [--check] [files ...]\n";
    assert_eq!(args(&["--bogus"]).unwrap_err().stderr, format!("{usage}picture_viewer.py: error: unrecognized arguments: --bogus\n"));
    assert_eq!(args(&["-x"]).unwrap_err().stderr, format!("{usage}picture_viewer.py: error: unrecognized arguments: -x\n"));
    assert_eq!(args(&["--fu=1"]).unwrap_err().stderr,
               format!("{usage}picture_viewer.py: error: argument -f/--fullscreen: ignored explicit argument '1'\n"));
    assert_eq!(check(), ("{\"ready\": true, \"gtk\": 4, \"application_id\": \"org.tessel.Pictures\", \
                          \"save_formats\": [\"BMP\", \"GIF\", \"JPEG\", \"PNG\", \"TIFF\", \"WEBP\"]}".to_string(), 0));
}

#[test]
fn unique_target_and_transfer_outcomes() {
    use crate::files::{transfer_files, unique_target, Decision};
    let dir = temp("transfer");
    let source = dir.0.join("photo.png");
    fs::write(&source, b"new").unwrap();
    let target_dir = dir.0.join("target");
    fs::create_dir(&target_dir).unwrap();
    let target = target_dir.join("photo.png");
    fs::write(&target, b"old").unwrap();
    assert_eq!(unique_target(&target).unwrap(), target_dir.join("photo (2).png"));
    let outcome = transfer_files(&[(source.clone(), target.clone(), Decision::Keep), (source.clone(), target.clone(), Decision::Skip),
                                   (source.clone(), source.clone(), Decision::Replace)], false, &[dir.0.join("later.png")]);
    let texts: Vec<&str> = outcome.iter().map(|o| o.2.as_str()).collect();
    assert_eq!(texts, ["Saved as photo (2).png", "Skipped", "Skipped: source and destination are the same file", "Cancelled"]);
    assert_eq!(fs::read(target_dir.join("photo (2).png")).unwrap(), b"new");
    assert_eq!(fs::read(&target).unwrap(), b"old");
    let moved = transfer_files(&[(source.clone(), target.clone(), Decision::Replace)], true, &[]);
    assert_eq!(moved[0].2, "Replaced");
    assert!(!source.exists());
    assert_eq!(fs::read(&target).unwrap(), b"new");
}
