//! Port of test_capture_media.py and test_capture_vulkan.py. FFmpeg runs only
//! on synthetic stdin pixels and writes into private temporary directories; no
//! desktop, PipeWire or PulseAudio connection is made.

use super::*;
use crate::native::{Feed, FeedError, StreamProps};
use std::sync::Arc;

fn solid_value(value: u16, hdr: bool, width: u32, height: u32) -> Frame {
    let mut pixel = Vec::new();
    for channel in [value, value, value, 65535] {
        pixel.extend_from_slice(&channel.to_le_bytes());
    }
    let frame = Frame::new(width, height, pixel.repeat((width * height) as usize));
    if hdr { frame.with("smpte2084", "bt2020", 10) } else { frame }
}

fn solid() -> Frame {
    solid_value(24000, false, 32, 24)
}

fn have_ffmpeg() -> bool {
    tessel_core::shell::which("ffmpeg").is_some() && tessel_core::shell::which("ffprobe").is_some()
}

fn temp(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("capture-media-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

fn pixel(png: &[u8], x: u32, y: u32) -> [u8; 3] {
    crate::imaging::decode_rgb(png).unwrap().get_pixel(x, y).0
}

fn after<'a>(command: &'a [String], flag: &str) -> &'a str {
    &command[command.iter().position(|a| a == flag).unwrap() + 1]
}

#[test]
fn test_presets_use_default_devices_and_never_silently_omit_audio() {
    let sources: Sources = vec![("other.monitor".into(), "Other speaker".into()), ("speaker.monitor".into(), "Default speaker".into()),
                                ("mic".into(), "Default microphone".into())];
    let answers = std::cell::RefCell::new(vec!["speaker\n".to_string(), "mic\n".to_string()]);
    let calls = std::cell::RefCell::new(Vec::<Vec<String>>::new());
    let command = |args: &[&str]| -> Result<String, ()> {
        calls.borrow_mut().push(args.iter().map(|s| s.to_string()).collect());
        Ok(answers.borrow_mut().remove(0))
    };
    assert_eq!(recording_audio_with(Some("desktop-mic"), "", "", sources.clone(), &command).unwrap(),
               (sources.clone(), "speaker.monitor".into(), "mic".into()));
    assert_eq!(calls.borrow()[0], ["pactl", "get-default-sink"]);
    assert_eq!(calls.borrow()[1], ["pactl", "get-default-source"]);
    let never = |_: &[&str]| -> Result<String, ()> { panic!("no pactl call expected") };
    let (_, system, mic) = recording_audio_with(Some("none"), "", "", sources.clone(), &never).unwrap();
    assert_eq!((system.as_str(), mic.as_str()), ("", ""));
    let (_, system, mic) = recording_audio_with(None, "other.monitor", "mic", sources.clone(), &never).unwrap();
    assert_eq!((system.as_str(), mic.as_str()), ("other.monitor", "mic"));
    assert!(recording_audio_with(None, "missing.monitor", "", sources.clone(), &never).unwrap_err().contains("unavailable"));
    let answers = std::cell::RefCell::new(vec!["speaker\n".to_string(), "speaker.monitor\n".to_string()]);
    let command = |_: &[&str]| -> Result<String, ()> { Ok(answers.borrow_mut().remove(0)) };
    assert!(recording_audio_with(Some("desktop-mic"), "", "", sources.clone(), &command).unwrap_err().contains("Select a microphone"));
    let failing = |_: &[&str]| -> Result<String, ()> { Err(()) };
    assert_eq!(recording_audio_with(Some("desktop"), "", "", sources, &failing).unwrap_err(),
               "The default audio device is unavailable. Check Audio Settings.");
    assert_eq!(parse_sources(br#"[{"name":"a.monitor","description":"Speakers"},{"name":"mic"}]"#),
               vec![("a.monitor".to_string(), "Speakers".to_string()), ("mic".to_string(), "mic".to_string())]);
    assert!(parse_sources(b"not json").is_empty());
}

#[test]
fn test_probe_uses_capture_size_crop_fps_and_preserves_errors() {
    if !have_ffmpeg() {
        return;
    }
    let frame = solid_value(24000, false, 256, 128);
    let options = RecordingOptions { encoder: "vulkan".into(), fps: 60, bitrate: 5, crop: Some((0, 0, 192, 96)), ..Default::default() };
    let command = encoder_probe_command(&frame, &options, &("vulkan".into(), Some(0))).unwrap();
    assert_eq!(after(&command, "-video_size"), "256x128");
    assert_eq!(after(&command, "-framerate"), "60");
    assert!(after(&command, "-vf").contains("crop=192:96:0:0"));
    assert!(!command.iter().any(|a| a == "-qp"), "Bitrate recording must not force constant QP");
    let inputs = std::cell::RefCell::new(Vec::new());
    let run = |_: &[String], input: &[u8], _: Duration| -> Result<process::Output, RunError> {
        inputs.borrow_mut().push(input.to_vec());
        Ok(process::Output { status: 1, stdout: Vec::new(), stderr: b"Hardware does not support encoding at size 32x24".to_vec() })
    };
    let error = choose_encoder_with(&options, &frame, &|_| Ok(vec![0]), &run).unwrap_err();
    assert!(error.contains("Hardware does not support encoding at size 32x24"), "{error}");
    assert!(error.contains("h264_vulkan (GPU 0): "), "{error}");
    assert_eq!(inputs.borrow().last().unwrap(), &frame.pixels);
}

#[test]
fn test_automatic_falls_back_to_openh264_when_vulkan_unavailable() {
    if !have_ffmpeg() {
        return;
    }
    let options = RecordingOptions { encoder: "auto".into(), ..Default::default() };
    assert_eq!(choose_encoder_with(&options, &solid(), &|_| Ok(vec![]), &process::run).unwrap(), ("cpu".to_string(), None));
    let command = recording_command(&solid(), &options, &("cpu".into(), None), "-").unwrap();
    assert_eq!(after(&command, "-c:v"), "libopenh264");
    assert!(!command.iter().any(|a| a == "-crf" || a == "-preset"));
    let error = choose_encoder_with(&RecordingOptions { encoder: "vulkan".into(), ..Default::default() }, &solid(), &|_| Ok(vec![]), &process::run)
        .unwrap_err();
    assert!(error.contains("No GPU driver exposes Vulkan Video encoding"), "{error}");
    // Enumeration failure: FFmpeg's own diagnostic is attempted with the default device.
    let probed = std::cell::RefCell::new(Vec::new());
    let run = |args: &[String], _: &[u8], _: Duration| -> Result<process::Output, RunError> {
        probed.borrow_mut().push(args.to_vec());
        Ok(process::Output { status: 1, stdout: Vec::new(), stderr: Vec::new() })
    };
    let error = choose_encoder_with(&RecordingOptions { encoder: "vulkan".into(), ..Default::default() }, &solid(),
                                    &|_| Err("libvulkan.so.1: cannot open shared object file".into()), &run).unwrap_err();
    assert!(error.starts_with("FFmpeg could not encode these recording settings.\n\nlibvulkan.so.1"), "{error}");
    assert!(error.ends_with("h264_vulkan: FFmpeg exited with status 1."), "{error}");
    assert_eq!(after(&probed.borrow()[0], "-init_hw_device"), "vulkan=capture");
}

#[test]
fn test_openh264_bitrate_mode_encodes() {
    if !have_ffmpeg() {
        return;
    }
    let options = RecordingOptions { encoder: "cpu".into(), bitrate: 5, fps: 60, ..Default::default() };
    let frame = solid_value(24000, false, 128, 96);
    let command = encoder_probe_command(&frame, &options, &("cpu".into(), None)).unwrap();
    assert_eq!(after(&command, "-c:v"), "libopenh264");
    assert_eq!(after(&command, "-rc_mode"), "bitrate");
    let result = process::run(&command, &frame.pixels, Duration::from_secs(15)).unwrap();
    assert_eq!(result.status, 0, "{}", String::from_utf8_lossy(&result.stderr));
}

#[test]
fn test_tonemap_changes_hdr_and_preserves_sdr_grayscale() {
    if !have_ffmpeg() {
        return;
    }
    let sdr = pixel(&screenshot(&solid(), None, 1000).unwrap(), 4, 4)[0] as f64;
    let hdr = pixel(&screenshot(&solid_value(24000, true, 32, 24), None, 1000).unwrap(), 4, 4)[0] as f64;
    assert!((sdr - 24000.0 / 65535.0 * 255.0).abs() <= 2.0, "{sdr}");
    assert!((sdr - hdr).abs() > 15.0);
    let levels: Vec<u8> = [0u16, 10000, 20000, 40000, 65535].iter()
        .map(|v| pixel(&screenshot(&solid_value(*v, true, 32, 24), None, 1000).unwrap(), 0, 0)[0]).collect();
    let mut sorted = levels.clone();
    sorted.sort();
    assert_eq!(levels, sorted);
    assert_eq!(levels[0], 0);
    assert!(*levels.last().unwrap() > 245);
}

#[test]
fn test_hlg_screenshot_and_pixel_crop() {
    if !have_ffmpeg() {
        return;
    }
    let frame = solid_value(24000, true, 32, 24).with("arib-std-b67", "bt2020", 10);
    let png = screenshot(&frame, Some((3, 4, 12, 8)), 1000).unwrap();
    assert_eq!(crate::imaging::decode_rgb(&png).unwrap().dimensions(), (12, 8));
    assert!(screenshot(&frame, Some((-2, 0, 5, 5)), 1000).is_err());
}

#[test]
fn test_hdr_requires_actual_hdr_metadata_and_precision() {
    for frame in [solid(), Frame::new(32, 24, solid().pixels).with("smpte2084", "bt2020", 8)] {
        assert!(color_filter(&frame, true, false, 1000, None).is_err());
    }
    assert!(RecordingOptions { codec: "h264".into(), hdr: true, ..Default::default() }.validate().is_err());
    // Exact filter text (capture_media.color_filter).
    assert_eq!(color_filter(&solid_value(1, true, 4, 4), false, true, 1050, None).unwrap(),
        "zscale=pin=bt2020:tin=smpte2084:min=gbr:rin=full:p=bt2020:t=linear:m=gbr:r=full:npl=100,format=gbrpf32le,zscale=p=bt709,\
tonemap=tonemap=mobius:param=0.3:desat=2:peak=10.5,zscale=pin=bt709:tin=linear:min=gbr:rin=full:p=bt709:t=iec61966-2-1:m=gbr:r=full,format=rgb24");
    assert_eq!(color_filter(&solid(), false, false, 1000, Some((0, 0, 4, 4))).unwrap(),
        "crop=4:4:0:0,zscale=pin=bt709:tin=iec61966-2-1:min=gbr:rin=full:p=bt709:t=bt709:m=bt709:r=limited,pad=ceil(iw/2)*2:ceil(ih/2)*2,format=yuv420p");
}

#[test]
fn test_workspace_negative_coordinates_and_mixed_output_scales() {
    if !have_ffmpeg() {
        return;
    }
    let image = screenshot(&solid(), None, 1000).unwrap();
    let props = |x, y, w, h| StreamProps { position: Some((x, y)), size: Some((w, h)), source_type: Some(1) };
    let output = compose_screens(vec![(image.clone(), props(-32, 0, 32, 24)), (image.clone(), props(0, 0, 16, 12))]).unwrap();
    assert_eq!(crate::imaging::decode_rgb(&output).unwrap().dimensions(), (48, 24));
    assert!(crate::imaging::icc_profile(&output).is_some());
    assert!(compose_screens(vec![(image.clone(), props(0, 0, 32, 24)), (image.clone(), props(0, 0, 32, 24))]).is_err());
    assert!(compose_screens(vec![(image.clone(), StreamProps::default()), (image, props(0, 0, 3, 3))]).unwrap_err().contains("screen positions"));
}

#[test]
fn test_native_packed_rgb_channels_and_hdr_tone_mapping() {
    if !have_ffmpeg() {
        return;
    }
    for (format, pixel_bytes) in [("bgr0", vec![0u8, 0, 255, 0]), ("x2rgb10le", (1023u32 << 20).to_le_bytes().to_vec()),
                                  ("x2bgr10le", 1023u32.to_le_bytes().to_vec())] {
        let depth = if format.contains("10") { 10 } else { 8 };
        let frame = Frame::new(16, 16, pixel_bytes.repeat(256)).with("iec61966-2-1", "bt709", depth).format(format);
        let [r, g, b] = pixel(&screenshot(&frame, None, 1000).unwrap(), 0, 0);
        assert!(r > 250 && g < 3 && b < 3, "{format}: {r} {g} {b}");
    }
    let packed = (500u32 | (500 << 10) | (500 << 20)).to_le_bytes().repeat(256);
    let frame = Frame::new(16, 16, packed).with("smpte2084", "bt2020", 10).format("x2rgb10le");
    let png = screenshot(&frame, None, 1000).unwrap();
    assert!(crate::imaging::icc_profile(&png).is_some());
    assert!((pixel(&png, 0, 0)[0] as f64 - 500.0 / 1023.0 * 255.0).abs() > 10.0);
}

#[test]
fn test_no_overwrite() {
    use std::os::unix::fs::PermissionsExt;
    let directory = temp("overwrite");
    let path = directory.join("shot.png");
    save_exclusive(&path, b"original").unwrap();
    assert_eq!(save_exclusive(&path, b"replacement").unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    let _ = std::fs::remove_dir_all(directory);
}

fn probe_json(path: &Path) -> serde_json::Value {
    let output = std::process::Command::new("ffprobe").args(["-v", "error", "-show_streams", "-of", "json"]).arg(path).output().unwrap();
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn test_all_codecs_and_hdr_metadata_with_real_encoders() {
    if !have_ffmpeg() {
        return;
    }
    let directory = temp("codecs");
    for (codec, hdr) in [("h264", false), ("h265", false), ("av1", false), ("h265", true), ("av1", true)] {
        let options = RecordingOptions { codec: codec.into(), hdr, encoder: "cpu".into(), container: "mp4".into(), fps: 10, ..Default::default() };
        let frame = solid_value(24000, hdr, 32, 24);
        let path = directory.join(format!("{codec}-{hdr}.mp4"));
        let command = recording_command(&frame, &options, &("cpu".into(), None), &path.to_string_lossy()).unwrap();
        let result = process::run(&command, &frame.pixels.repeat(3), Duration::from_secs(20)).unwrap();
        assert_eq!(result.status, 0, "{}", String::from_utf8_lossy(&result.stderr));
        let video = &probe_json(&path)["streams"][0];
        assert_eq!(video["codec_name"], match codec { "h264" => "h264", "h265" => "hevc", _ => "av1" });
        assert_eq!(video["color_transfer"], if hdr { "smpte2084" } else { "bt709" });
        assert_eq!(video["color_primaries"], if hdr { "bt2020" } else { "bt709" });
        assert!(video["pix_fmt"].as_str().unwrap().contains(if hdr { "10" } else { "yuv420p" }));
    }
    let _ = std::fs::remove_dir_all(directory);
}

struct StaticFeed(StreamProps);

impl Feed for StaticFeed {
    fn frame(&self, _timeout: f64, _not_before: Option<f64>) -> Result<Frame, FeedError> {
        Ok(solid())
    }
    fn properties(&self) -> &StreamProps {
        &self.0
    }
    fn close(&self) {}
}

#[test]
fn test_recording_finalizes_and_respects_wall_clock() {
    if !have_ffmpeg() {
        return;
    }
    let directory = temp("recording");
    let path = directory.join("recording.mkv");
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let options = RecordingOptions { encoder: "cpu".into(), duration: 1, fps: 10, ..Default::default() };
    let mut recorder = Recorder::with_encoder_chooser(Arc::new(StaticFeed(StreamProps::default())), options, path.clone(),
        Box::new(move |event| sink.lock().unwrap().push(event)),
        Box::new(|options, frame| choose_encoder_with(options, frame, &|_| Ok(vec![]), &process::run)));
    assert!(recorder.join(Duration::from_secs(15)));
    let events = events.lock().unwrap().clone();
    assert!(matches!(events.last(), Some(RecordEvent::Saved(_))), "{events:?}");
    assert_eq!(events[0], RecordEvent::Recording("cpu".into()));
    let output = std::process::Command::new("ffprobe").args(["-v", "error", "-show_entries", "format=duration", "-of",
        "default=noprint_wrappers=1:nokey=1"]).arg(&path).output().unwrap();
    let duration: f64 = String::from_utf8_lossy(&output.stdout).trim().parse().unwrap();
    assert!(duration > 0.7 && duration < 1.5, "{duration}");
    assert!(!std::fs::read_dir(&directory).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".recording-")));
    // Never overwrite: a second recording to the same name fails and keeps the partial file.
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let options = RecordingOptions { encoder: "cpu".into(), duration: 1, fps: 10, ..Default::default() };
    let mut recorder = Recorder::with_encoder_chooser(Arc::new(StaticFeed(StreamProps::default())), options, path.clone(),
        Box::new(move |event| sink.lock().unwrap().push(event)),
        Box::new(|_, _| Ok(("cpu".to_string(), None))));
    assert!(recorder.join(Duration::from_secs(15)));
    let last = events.lock().unwrap().last().cloned().unwrap();
    assert!(matches!(&last, RecordEvent::Error(text) if text.contains("File exists") && text.contains("\nPartial recording: ")), "{last:?}");
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn test_options_validation_messages_and_encoder_arguments() {
    let base = RecordingOptions::default();
    assert!(base.validate().is_ok());
    assert_eq!(RecordingOptions { container: "avi".into(), ..base.clone() }.validate().unwrap_err(), "Unsupported codec or container.");
    assert_eq!(RecordingOptions { fps: 0, ..base.clone() }.validate().unwrap_err(), "Invalid frame rate, bitrate or quality.");
    assert_eq!(RecordingOptions { peak: 50, ..base.clone() }.validate().unwrap_err(), "Invalid encoder or HDR peak brightness.");
    assert_eq!(RecordingOptions { audio: vec!["a".into(), "b".into(), "c".into()], ..base.clone() }.validate().unwrap_err(),
               "Choose at most two valid audio sources.");
    assert_eq!(RecordingOptions { duration: 86401, ..base.clone() }.validate().unwrap_err(),
               "The recording timer must be between 0 and 24 hours.");
    let (before, filters, args) = encoder_arguments(&RecordingOptions { codec: "h265".into(), hdr: true, ..base.clone() }, "vulkan", Some(1)).unwrap();
    assert_eq!(before, ["-init_hw_device", "vulkan=capture:1", "-filter_hw_device", "capture"]);
    assert_eq!(filters, ["format=p010le", "hwupload"]);
    assert_eq!(args, ["-c:v", "hevc_vulkan", "-usage", "record", "-content", "desktop", "-rc_mode", "cqp", "-qp", "23"]);
    let (_, _, args) = encoder_arguments(&RecordingOptions { codec: "av1".into(), bitrate: 8, ..base.clone() }, "cpu", None).unwrap();
    assert_eq!(args, ["-c:v", "libsvtav1", "-preset", "8", "-svtav1-params", "lp=4", "-b:v", "8M", "-maxrate", "8M", "-bufsize", "16M"]);
    let options = RecordingOptions { codec: "h265".into(), container: "mp4".into(), audio: vec!["a.monitor".into(), "mic".into()], duration: 5, ..base };
    let command = recording_command(&solid(), &options, &("cpu".into(), None), "/x/out.mp4").unwrap();
    let text = command.join(" ");
    assert!(text.starts_with("ffmpeg -hide_banner -loglevel warning -n -threads 4 -filter_threads 2 -use_wallclock_as_timestamps 1 -f rawvideo \
-pixel_format rgba64le -video_size 32x24 -framerate 30 -i pipe:0 -thread_queue_size 128 -f pulse -i a.monitor -thread_queue_size 128 -f pulse -i mic \
-map 0:v:0 -filter_complex [1:a][2:a]amix=inputs=2:duration=longest,aresample=async=1:first_pts=0[a] -map [a] -vf "), "{text}");
    assert!(text.ends_with("-c:v libx265 -preset fast -crf 23 -x265-params pools=4:frame-threads=2:log-level=error -threads 4 -r 30 -fps_mode cfr -g 60 \
-color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv -c:a aac -b:a 192k -shortest -tag:v hvc1 -movflags +faststart -t 5 /x/out.mp4"), "{text}");
}

/// test_capture_vulkan.py — opt-in hardware test (CAPTURE_TEST_VULKAN=1), synthetic pixels only.
#[test]
fn test_vulkan_codecs_bitrate_quality_hdr_and_sdr_conversion() {
    if std::env::var("CAPTURE_TEST_VULKAN").as_deref() != Ok("1") || !have_ffmpeg() {
        return;
    }
    let directory = temp("vulkan");
    for codec in ["h264", "h265", "av1"] {
        let modes: &[bool] = if codec == "h264" { &[false] } else { &[false, true] };
        for &hdr in modes {
            for bitrate in [0, 5] {
                let frame = Frame::new(1920, 1080, vec![128u8, 128, 128, 0].repeat(1920 * 1080))
                    .with(if hdr { "smpte2084" } else { "iec61966-2-1" }, if hdr { "bt2020" } else { "bt709" }, if hdr { 10 } else { 8 })
                    .format(if hdr { "x2rgb10le" } else { "bgr0" });
                let mut options = RecordingOptions { codec: codec.into(), hdr, encoder: "vulkan".into(), fps: 60, bitrate, ..Default::default() };
                let encoder = choose_encoder(&options, &frame).unwrap();
                assert_eq!(encoder.0, "vulkan");
                let path = directory.join(format!("{codec}-{hdr}-{bitrate}.mkv"));
                let command = recording_command(&frame, &options, &encoder, &path.to_string_lossy()).unwrap();
                let result = process::run(&command, &frame.pixels.repeat(3), Duration::from_secs(30)).unwrap();
                assert_eq!(result.status, 0, "{}", String::from_utf8_lossy(&result.stderr));
                let metadata = &probe_json(&path)["streams"][0];
                assert_eq!(metadata["color_transfer"], if hdr { "smpte2084" } else { "bt709" });
                if hdr {
                    assert!(metadata["pix_fmt"].as_str().unwrap().contains("10"));
                    options.hdr = false;
                    assert_eq!(choose_encoder(&options, &frame).unwrap().0, "vulkan");
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(directory);
}
