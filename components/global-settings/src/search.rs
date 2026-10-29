//! `settings_search.py`: searchable setting metadata without importing or
//! starting page backends.
use std::collections::HashMap;
use std::sync::LazyLock;

use gs_base::Setting;
use unicode_normalization::{char::canonical_combining_class, UnicodeNormalization};

static WORDS: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[\p{L}\p{N}]+").unwrap());

/// NFKD, casefold, drop combining marks, keep `[^\W_]+` tokens joined by spaces.
pub fn normalize(text: &str) -> String {
    let folded = suite_core::pyjson::casefold(&text.nfkd().collect::<String>());
    let stripped: String = folded.chars().filter(|c| canonical_combining_class(*c) == 0).collect();
    WORDS.find_iter(&stripped).map(|m| m.as_str()).collect::<Vec<_>>().join(" ")
}

/// Titles/labels correspond to existing controls (178 entries, Python order).
pub static SETTINGS: &[Setting] = &[
    Setting { section: "login", key: "picture", title: "Login screen wallpaper", detail: "Choose the system login screen background.", keywords: "greeter login wallpaper image background", labels: &[], target: "picture", subpage: "" },
    Setting { section: "login", key: "blur_strength", title: "Login screen blur and transparency", detail: "Adjust the login screen appearance.", keywords: "greeter blur transparency", labels: &[], target: "blur_strength", subpage: "" },
    Setting { section: "login", key: "idle_seconds", title: "Login screen idle clock", detail: "Choose idle timing and 12 or 24-hour time.", keywords: "greeter idle clock time", labels: &[], target: "idle_seconds", subpage: "" },
    Setting { section: "login", key: "default_session", title: "Default login session", detail: "Choose an installed Wayland desktop session.", keywords: "greeter session labwc desktop", labels: &[], target: "default_session", subpage: "" },
    Setting { section: "keyboard", key: "automatic", title: "Automatic on-screen keyboard", detail: "Show for touchscreens when no physical keyboard is connected.", keywords: "touch screen virtual keyboard typing automatic", labels: &[], target: "keyboard-automatic", subpage: "" },
    Setting { section: "keyboard", key: "layout", title: "On-screen keyboard layout", detail: "Choose the language and key arrangement.", keywords: "qwerty azerty qwertz language virtual keyboard", labels: &[], target: "keyboard-layout", subpage: "" },
    Setting { section: "keyboard", key: "size", title: "On-screen keyboard key size", detail: "Choose small, medium, or large touch keys.", keywords: "touch virtual keyboard size large", labels: &[], target: "keyboard-size", subpage: "" },
    Setting { section: "keyboard", key: "keyboard-layouts", title: "Keyboard layouts", detail: "Add, remove and reorder physical keyboard layouts.", keywords: "keyboard layout language input xkb qwerty azerty qwertz dvorak colemak add remove order variant", labels: &["Layouts"], target: "", subpage: "" },
    Setting { section: "keyboard", key: "keyboard-switch", title: "Switch keyboard layout shortcut", detail: "Choose the keys that change to the next keyboard layout.", keywords: "keyboard layout switch toggle shortcut alt shift caps lock group", labels: &["Switch layouts with"], target: "", subpage: "" },
    Setting { section: "keyboard", key: "keyboard-compose", title: "Compose key", detail: "Type accented letters and symbols with a compose key sequence.", keywords: "keyboard compose multi key accent special characters symbols", labels: &["Compose key"], target: "", subpage: "" },
    Setting { section: "keyboard", key: "keyboard-repeat-delay", title: "Key repeat delay", detail: "Choose how long to hold a key before it repeats.", keywords: "keyboard repeat delay hold typing milliseconds", labels: &["Repeat delay"], target: "", subpage: "" },
    Setting { section: "keyboard", key: "keyboard-repeat-rate", title: "Key repeat rate", detail: "Choose how quickly a held key repeats.", keywords: "keyboard repeat rate speed typing", labels: &["Repeat rate"], target: "", subpage: "" },
    Setting { section: "keyboard", key: "devices", title: "Physical keyboard detection", detail: "Ignore receivers that incorrectly identify as keyboards.", keywords: "touch virtual keyboard receiver devices ignore", labels: &[], target: "keyboard-devices", subpage: "" },
    Setting { section: "input", key: "speed", title: "Pointer speed", detail: "Adjust mouse and touchpad movement speed per device.", keywords: "mouse touchpad sensitivity pointer cursor speed", labels: &[], target: "pointerSpeed", subpage: "" },
    Setting { section: "input", key: "polling", title: "Polling rate", detail: "Read the current mouse polling rate and adjust it on supported devices.", keywords: "mouse polling report rate frequency hz hertz usb razer libratbag", labels: &[], target: "pollingRate", subpage: "" },
    Setting { section: "input", key: "acceleration", title: "Acceleration", detail: "Choose adaptive acceleration or a flat movement multiplier.", keywords: "mouse touchpad pointer acceleration adaptive flat", labels: &[], target: "accelProfile", subpage: "" },
    Setting { section: "input", key: "primary", title: "Primary button", detail: "Swap left and right mouse buttons.", keywords: "mouse touchpad left handed primary button right", labels: &[], target: "leftHanded", subpage: "" },
    Setting { section: "input", key: "middle", title: "Emulate middle click", detail: "Press both mouse buttons to produce a middle click.", keywords: "mouse touchpad middle emulation", labels: &[], target: "middleEmulation", subpage: "" },
    Setting { section: "input", key: "natural", title: "Natural scrolling", detail: "Reverse the scrolling direction for the selected device.", keywords: "mouse touchpad natural reverse inverted scroll direction", labels: &[], target: "naturalScroll", subpage: "" },
    Setting { section: "input", key: "scroll_speed", title: "Scroll speed", detail: "Adjust wheel and touchpad scrolling speed.", keywords: "mouse touchpad wheel scroll speed multiplier", labels: &[], target: "scrollFactor", subpage: "" },
    Setting { section: "input", key: "scroll_method", title: "Scroll method", detail: "Choose two-finger scrolling, edge scrolling, or button scrolling.", keywords: "touchpad two finger edge scroll mouse wheel trackpoint button", labels: &[], target: "scrollMethod", subpage: "" },
    Setting { section: "input", key: "tap", title: "Tap to click", detail: "Configure one, two, and three-finger taps.", keywords: "touchpad tap click gesture fingers", labels: &[], target: "tap", subpage: "" },
    Setting { section: "input", key: "areas", title: "Physical click method", detail: "Use clickpad button areas or finger-count clicks.", keywords: "touchpad clickpad area click bottom left right finger", labels: &[], target: "clickMethod", subpage: "" },
    Setting { section: "input", key: "drag", title: "Tap and drag", detail: "Configure tap-and-drag and drag lock.", keywords: "touchpad tap drag lock gesture", labels: &[], target: "tapAndDrag", subpage: "" },
    Setting { section: "input", key: "gesture", title: "Multi-finger drag", detail: "Drag using three or four fingers on supported touchpads.", keywords: "touchpad gestures swipe three four finger drag", labels: &[], target: "threeFingerDrag", subpage: "" },
    Setting { section: "input", key: "typing", title: "Disable while typing", detail: "Prevent accidental touchpad movement while typing.", keywords: "touchpad palm rejection typing keyboard", labels: &[], target: "disableWhileTyping", subpage: "" },
    Setting { section: "input", key: "state", title: "Device state", detail: "Enable input or disable a touchpad while an external mouse is connected.", keywords: "mouse touchpad enable disable external connected", labels: &[], target: "sendEventsMode", subpage: "" },
    Setting { section: "lock", key: "wallpaper", title: "Lock screen wallpaper", detail: "Use the desktop wallpaper, a picture, slideshow, or solid color.", keywords: "lock screen background photo picture slideshow", labels: &[], target: "", subpage: "" },
    Setting { section: "lock", key: "blur", title: "Lock screen blur", detail: "Follow application appearance or choose a separate blur strength.", keywords: "lock screen blur frost strength", labels: &[], target: "", subpage: "" },
    Setting { section: "lock", key: "transparency", title: "Lock screen transparency", detail: "Adjust the sign-in panel transparency.", keywords: "lock screen opacity transparent", labels: &[], target: "", subpage: "" },
    Setting { section: "lock", key: "automatic", title: "Automatic locking", detail: "Set the idle delay; Keep Awake and idle inhibitors block automatic locking.", keywords: "lock screen timeout idle seconds security", labels: &[], target: "", subpage: "" },
    Setting { section: "lock", key: "weather", title: "Lock screen weather", detail: "Show current conditions for the location saved in Weather settings.", keywords: "lock screen current weather location temperature", labels: &[], target: "", subpage: "" },
    Setting { section: "lock", key: "forecast", title: "Lock screen forecast", detail: "Show or hide the seven-day forecast on the lock screen.", keywords: "lock screen weather forecast 7 day seven days", labels: &[], target: "", subpage: "" },
    Setting { section: "lock", key: "pause_media", title: "Pause media when locking", detail: "Pause current playback or let it continue when the screen locks.", keywords: "lock screen media music video playback pause continue", labels: &[], target: "", subpage: "" },
    Setting { section: "lock", key: "media", title: "Lock screen media controls", detail: "Show the current media title and a play/pause button.", keywords: "lock screen title media music video play pause controls", labels: &[], target: "", subpage: "" },
    Setting { section: "audio", key: "outputs", title: "Output Devices", detail: "Choose speakers, headphones, volume, and the default output.", keywords: "sound playback speaker headphone headset volume mute balance channels", labels: &[], target: "", subpage: "outputs" },
    Setting { section: "audio", key: "inputs", title: "Input Devices", detail: "Choose a microphone and adjust recording volume.", keywords: "sound microphone mic input mute gain channels", labels: &[], target: "", subpage: "inputs" },
    Setting { section: "audio", key: "playback", title: "Playback", detail: "Adjust the volume and output of individual applications.", keywords: "sound app volume mixer routing", labels: &[], target: "", subpage: "playback" },
    Setting { section: "audio", key: "recording", title: "Recording", detail: "Choose the input used by recording applications.", keywords: "sound microphone app stream capture", labels: &[], target: "", subpage: "recording" },
    Setting { section: "audio", key: "configuration", title: "Configuration", detail: "Select an audio device profile.", keywords: "sound hardware card profile surround stereo hdmi", labels: &[], target: "", subpage: "configuration" },
    Setting { section: "audio", key: "loopbacks", title: "Loopbacks", detail: "Add or remove persistent virtual audio outputs.", keywords: "sound virtual device loopback redirect pipewire", labels: &["Add Loopback", "Loopbacks"], target: "", subpage: "loopbacks" },
    Setting { section: "bluetooth", key: "devices", title: "Known Devices", detail: "Connect, disconnect, forget, or trust Bluetooth devices.", keywords: "bluetooth pair pairing headset controller mouse keyboard trust remove", labels: &["Known Devices"], target: "", subpage: "" },
    Setting { section: "bluetooth", key: "add", title: "Add Device", detail: "Scan for nearby Bluetooth devices to pair.", keywords: "bluetooth scan discover new adapter", labels: &["Add Device", "Add device"], target: "", subpage: "" },
    Setting { section: "bluetooth", key: "receive", title: "Receive files", detail: "Accept files sent to this computer over Bluetooth.", keywords: "bluetooth receive incoming obex file transfer send to computer share", labels: &["Receive files"], target: "", subpage: "" },
    Setting { section: "network", key: "ethernet", title: "Ethernet/LAN", detail: "Manage wired network adapters and connections.", keywords: "network internet wired ethernet lan cable", labels: &["Ethernet/LAN"], target: "", subpage: "connections" },
    Setting { section: "network", key: "wifi", title: "Wi-Fi", detail: "Enable wireless adapters and connect to nearby networks.", keywords: "wifi wi fi wireless internet ssid password scan", labels: &["Wi-Fi"], target: "", subpage: "connections" },
    Setting { section: "network", key: "vpn", title: "VPN", detail: "Manage VPN connections and view externally managed tunnels.", keywords: "vpn wireguard openvpn mullvad tunnel private network", labels: &["VPN"], target: "", subpage: "connections" },
    Setting { section: "network", key: "saved", title: "Saved connections", detail: "Add or edit connection profiles, IP addresses, DNS, and routing.", keywords: "network ip ipv4 ipv6 dns dhcp proxy static route profile password", labels: &["Saved connections", "Search connections"], target: "search_entry", subpage: "saved" },
    Setting { section: "network", key: "add", title: "Add connection", detail: "Create a Wi-Fi, Ethernet, VPN, or virtual connection profile.", keywords: "new network bridge bond vlan vpn import", labels: &[], target: "add_button", subpage: "saved" },
    Setting { section: "displays", key: "resolution", title: "Resolution", detail: "Choose a monitor's resolution and refresh rate.", keywords: "display screen monitor resolution refresh rate hz mode", labels: &["Resolution", "Mode"], target: "", subpage: "" },
    Setting { section: "displays", key: "scale", title: "Scale Override", detail: "Change the size of text and controls on each monitor.", keywords: "display screen dpi scaling fractional zoom size", labels: &["Scale Override"], target: "", subpage: "" },
    Setting { section: "displays", key: "orientation", title: "Orientation", detail: "Rotate or flip a display.", keywords: "display rotation portrait landscape flip", labels: &["Orientation"], target: "", subpage: "" },
    Setting { section: "displays", key: "primary", title: "Primary", detail: "Choose the primary display.", keywords: "display primary main screen monitor", labels: &["Primary"], target: "", subpage: "" },
    Setting { section: "displays", key: "vrr", title: "Adaptive Sync (VRR)", detail: "Configure variable refresh rate for each monitor.", keywords: "display vrr freesync gsync adaptive sync refresh", labels: &["Adaptive Sync (VRR)"], target: "", subpage: "" },
    Setting { section: "displays", key: "color_profile", title: "SDR display profile", detail: "Assign a calibrated ICC display profile.", keywords: "display color colour icc icm profile photo video editing calibration gamut", labels: &["SDR display profile"], target: "", subpage: "" },
    Setting { section: "displays", key: "calibration", title: "Apply calibration curves", detail: "Apply the ICC profile’s video card calibration.", keywords: "display color vcgt gamma calibration curves", labels: &["Apply calibration curves"], target: "", subpage: "" },
    Setting { section: "displays", key: "color_depth", title: "SDR color depth", detail: "Choose 8-bit or 10-bit SDR output independently of HDR.", keywords: "display color bit depth 10 bit banding precision photo video", labels: &["SDR color depth"], target: "", subpage: "" },
    Setting { section: "displays", key: "hdr", title: "HDR", detail: "Configure high dynamic range and view reported peak brightness and reference white.", keywords: "display hdr high dynamic range colors nits luminance peak maximum brightness reference white", labels: &["HDR"], target: "", subpage: "" },
    Setting { section: "displays", key: "sdr_editing_hdr", title: "SDR editing with HDR…", detail: "Use SDR editing presets while keeping HDR enabled.", keywords: "sdr hdr rec709 srgb photo video white reference correction workspace", labels: &["SDR editing with HDR…"], target: "", subpage: "" },
    Setting { section: "displays", key: "hdr_calibration", title: "Calibrate HDR…", detail: "Set per-monitor HDR limits and SDR reference white.", keywords: "hdr calibration nits clipping peak full screen paper white luminance photo video editing", labels: &["Calibrate HDR…"], target: "", subpage: "" },
    Setting { section: "displays", key: "brightness", title: "Brightness", detail: "Adjust brightness on supported displays.", keywords: "display screen backlight dim light", labels: &["Brightness"], target: "", subpage: "" },
    Setting { section: "displays", key: "contrast", title: "Contrast", detail: "Adjust contrast on supported displays.", keywords: "display screen contrast ddc", labels: &["Contrast"], target: "", subpage: "" },
    Setting { section: "displays", key: "dim", title: "Automatically dim when idle", detail: "Dim supported displays after inactivity.", keywords: "display auto dim idle brightness timeout", labels: &["Automatically dim when idle"], target: "", subpage: "" },
    Setting { section: "displays", key: "identify", title: "Identify displays", detail: "Show a label on each connected display.", keywords: "display identify screen monitor arrange position", labels: &["Identify displays"], target: "", subpage: "" },
    Setting { section: "power", key: "profile", title: "Power profile", detail: "Choose performance, balanced, or power saving behavior.", keywords: "power performance balanced saver cpu energy", labels: &["Power profile"], target: "", subpage: "" },
    Setting { section: "power", key: "display_off", title: "Turn off display after", detail: "Set display-off delays for plugged-in and battery power.", keywords: "power screen monitor off blank idle timeout plugged battery", labels: &["Turn off display after"], target: "", subpage: "" },
    Setting { section: "power", key: "sleep", title: "Sleep after", detail: "Set automatic sleep delays for each power source.", keywords: "power suspend sleep idle timeout plugged battery", labels: &["Sleep after"], target: "", subpage: "" },
    Setting { section: "power", key: "sleep_mode", title: "Sleep mode", detail: "Choose suspend or hibernate behavior.", keywords: "power hibernate suspend sleep hybrid", labels: &["Sleep mode"], target: "", subpage: "" },
    Setting { section: "power", key: "lock", title: "Lock after", detail: "Lock the screen after inactivity.", keywords: "power lock screen security idle timeout", labels: &["Lock after"], target: "lock_choice", subpage: "" },
    Setting { section: "battery", key: "status", title: "Battery", detail: "View device battery levels, charging, and plugged-in status.", keywords: "battery charge charging full low peripheral bluetooth mouse keyboard", labels: &["Battery"], target: "", subpage: "" },
    Setting { section: "battery", key: "charge", title: "Maximum charge", detail: "Limit charging on supported laptop batteries.", keywords: "battery maximum charge limit threshold percent health lifespan eighty 80", labels: &["Maximum charge"], target: "", subpage: "" },
    Setting { section: "keepawake", key: "enabled", title: "Keep this session awake", detail: "Pause automatic dimming, display-off, and sleep.", keywords: "inhibit idle awake prevent sleep screensaver presentation caffeine", labels: &["Keep this session awake"], target: "", subpage: "" },
    Setting { section: "notifications", key: "dnd", title: "Do Not Disturb", detail: "Keep notifications in history without showing popups.", keywords: "notifications do not disturb dnd silent mute alerts", labels: &["Do Not Disturb"], target: "dnd_switch", subpage: "" },
    Setting { section: "notifications", key: "fullscreen", title: "Pause during fullscreen", detail: "Hide notification popups while fullscreen applications are focused.", keywords: "notifications fullscreen game gaming borderless popup", labels: &["Pause during fullscreen"], target: "fullscreen_switch", subpage: "" },
    Setting { section: "notifications", key: "dnd_duration", title: "Do Not Disturb duration", detail: "Turn on Do Not Disturb for 1 hour, until tomorrow, or until turned off.", keywords: "notifications do not disturb dnd duration hour tomorrow until timer silent", labels: &["Do Not Disturb duration", "For 1 hour", "Until tomorrow", "Until turned off"], target: "", subpage: "" },
    Setting { section: "notifications", key: "applications", title: "Application notifications", detail: "Mute notifications from individual applications.", keywords: "notifications applications apps per app mute silence block", labels: &["Applications", "Mute"], target: "", subpage: "" },
    Setting { section: "notifications", key: "duration", title: "Popup duration", detail: "Choose how long normal notification popups remain visible.", keywords: "notifications timeout duration seconds popup", labels: &["Popup duration"], target: "timeout_spin", subpage: "" },
    Setting { section: "layout", key: "presets", title: "Desktop Layout", detail: "Apply a preset arrangement of the dock, panel and desktop.", keywords: "layout preset windows win taskbar start menu arrangement dock panel bar", labels: &["Desktop Layout"], target: "presets", subpage: "" },
    Setting { section: "theme", key: "wallpaper", title: "Wallpaper", detail: "Choose an image, video, placement, or background color for each display.", keywords: "theme background wallpaper picture photo video browse add remove desktop", labels: &["Image or video", "Placement", "Theme & Wallpaper"], target: "", subpage: "" },
    Setting { section: "theme", key: "match", title: "Match wallpaper colors", detail: "Match the shell and applications to the wallpaper palette.", keywords: "theme palette accent automatic colors colour papirus folder", labels: &["Match wallpaper colors"], target: "match", subpage: "" },
    Setting { section: "theme", key: "dock_transparency", title: "Dock transparency", detail: "Adjust dock transparency independently of application windows.", keywords: "theme dock bar panel opacity transparent", labels: &["Dock transparency"], target: "dock_transparency", subpage: "" },
    Setting { section: "theme", key: "app_transparency", title: "Application transparency", detail: "Adjust app and menu transparency while keeping text and icons opaque.", keywords: "theme app application window opacity transparent legibility", labels: &["Application transparency"], target: "apps_transparency", subpage: "" },
    Setting { section: "theme", key: "dock_frost", title: "Dock Blur", detail: "Blur the content behind the dock.", keywords: "theme dock panel bar frost frosted blur mica glass strength", labels: &["Blur"], target: "frosted", subpage: "" },
    Setting { section: "theme", key: "app_frost", title: "Application Blur", detail: "Blur the content behind applications and menus.", keywords: "theme app application window frost frosted blur mica glass strength", labels: &["Blur"], target: "apps_frosted", subpage: "" },
    Setting { section: "theme", key: "dock_frost_strength", title: "Dock Blur strength", detail: "Adjust the dock background blur strength.", keywords: "theme dock panel frost frosted blur mica strength", labels: &["Blur strength"], target: "frost_strength", subpage: "" },
    Setting { section: "theme", key: "app_frost_strength", title: "Application Blur strength", detail: "Adjust application background blur strength.", keywords: "theme app application window frost frosted blur mica strength", labels: &["Blur strength"], target: "apps_frost_strength", subpage: "" },
    Setting { section: "theme", key: "corners", title: "Rounded corners", detail: "Round application windows and desktop menus.", keywords: "theme application app round rounded corner square", labels: &["Rounded corners"], target: "apps_rounded_corners", subpage: "" },
    Setting { section: "behavior", key: "workspaces", title: "Number of workspaces", detail: "Choose how many virtual desktops are available across your monitors.", keywords: "desktop behavior behaviour workspace workspaces virtual desktops spaces count number add remove maximum limit", labels: &["Number of workspaces"], target: "workspace_count", subpage: "" },
    Setting { section: "behavior", key: "gapless", title: "Gapless", detail: "Remove spacing between tiled windows and at screen edges.", keywords: "desktop behavior behaviour tiling tile gapless gaps spacing windows snap edge margin", labels: &["Gapless"], target: "gapless_switch", subpage: "" },
    Setting { section: "behavior", key: "hot_corners", title: "Hot corners", detail: "Open Activities by hovering in selected screen corners.", keywords: "desktop behavior behaviour hot corner activities alt tab switch windows", labels: &["Hot corners"], target: "", subpage: "" },
    Setting { section: "behavior", key: "corner_delay", title: "Activation delay", detail: "Adjust the delay before a hot corner opens Activities.", keywords: "desktop hot corner delay speed milliseconds", labels: &["Activation delay"], target: "hot_corner_delay", subpage: "" },
    Setting { section: "behavior", key: "fullscreen_menu", title: "Menu hotkey in fullscreen", detail: "Allow the application menu shortcut in fullscreen applications.", keywords: "desktop menu hotkey shortcut fullscreen game borderless super windows key", labels: &["Menu hotkey in fullscreen"], target: "menu_hotkey_fullscreen", subpage: "" },
    Setting { section: "dock", key: "mode", title: "Layout mode", detail: "Choose a compact dock or a full-width bar.", keywords: "dock panel bar layout mode taskbar", labels: &["Layout mode"], target: "mode", subpage: "" },
    Setting { section: "dock", key: "monitor", title: "Show dock on", detail: "Select one monitor or clone the dock on all monitors.", keywords: "dock panel bar display monitor clone screen", labels: &["Show dock on"], target: "monitor", subpage: "" },
    Setting { section: "dock", key: "placement", title: "Placement", detail: "Choose screen edge and alignment for each dock.", keywords: "dock panel bar position edge alignment top bottom left right center", labels: &["Placement"], target: "", subpage: "" },
    Setting { section: "dock", key: "size", title: "Icon size", detail: "Adjust dock icon size.", keywords: "dock panel bar icon size pixels small large", labels: &["Icon size"], target: "size", subpage: "" },
    Setting { section: "dock", key: "float", title: "Float mode", detail: "Keep a gap between the dock and screen edges.", keywords: "dock panel bar floating gap margin space edge", labels: &["Float mode"], target: "float_mode", subpage: "" },
    Setting { section: "dock", key: "corners", title: "Rounded corners", detail: "Round the dock outline.", keywords: "dock panel bar round rounded corner square", labels: &["Rounded corners"], target: "rounded_corners", subpage: "" },
    Setting { section: "dock", key: "autohide", title: "Auto-hide when inactive", detail: "Hide the dock until the pointer moves over its area.", keywords: "dock panel bar auto hide autohide reveal inactive", labels: &["Auto-hide when inactive"], target: "auto_hide", subpage: "" },
    Setting { section: "dock", key: "behind", title: "Keep behind windows", detail: "Keep the dock behind windows and reveal it on hover outside fullscreen.", keywords: "dock panel bar fullscreen maximize hover behind windows", labels: &["Keep behind windows"], target: "fullscreen", subpage: "" },
    Setting { section: "dock", key: "application_display", title: "Application display", detail: "Show application icons or the focused window title.", keywords: "dock panel bar application icons focused title taskbar", labels: &["Application display"], target: "application_display", subpage: "" },
    Setting { section: "dock", key: "separate_app_dock", title: "Separate app dock", detail: "Move application controls out of a top bar. Choose left, right or bottom placement, full length, icon size, floating, rounded corners, transparency, blur, background color and auto-hide independently.", keywords: "dock panel bar separate sidebar launcher applications full height width transparency blur appearance color colour gray grey picker", labels: &["Separate app dock", "Use separate app dock"], target: "separate_app_dock", subpage: "" },
    Setting { section: "dock", key: "background_color", title: "Dock / bar background color", detail: "Choose a background color when wallpaper color matching is off.", keywords: "dock bar panel background color colour picker appearance", labels: &[], target: "background_color", subpage: "" },
    Setting { section: "dock", key: "bar_border", title: "Bar border", detail: "Show a border on any of the bar's top, bottom, left or right edges. Follow wallpaper colors or choose a custom color.", keywords: "dock panel bar border outline sides edges color colour matching custom picker", labels: &[], target: "bar_border_enabled", subpage: "" },
    Setting { section: "dock", key: "app_dock_group_layout", title: "App dock group positions and order", detail: "Position and reorder the separate dock's application menu, pinned apps, open windows, and Home, Settings and Trash independently of the bar.", keywords: "separate dock layout group position order move up down top left center bottom right pinned apps menu home trash", labels: &["App dock group positions and order"], target: "app_dock_layout_controls", subpage: "" },
    Setting { section: "dock", key: "groups", title: "Visible groups", detail: "Show or hide menu, pinned apps, workspaces, tray, applets, and clock groups.", keywords: "dock panel bar groups show hide pinned apps workspace tray weather menu clock home trash settings", labels: &["Visible groups"], target: "", subpage: "" },
    Setting { section: "dock", key: "group_layout", title: "Group positions and order", detail: "Assign groups to Top/Left, Center or Bottom/Right and reorder them within each position.", keywords: "layout position order move groups up down top left center bottom right weather workspaces home trash pinned activities focused title clock time date media music playback tray default apps session", labels: &["Group positions and order", "Reset layout"], target: "", subpage: "" },
    Setting { section: "dock", key: "default_apps", title: "Always-visible applets", detail: "Choose which applet icons stay visible when the applets collapse.", keywords: "always visible applets icons hover expand collapse sound audio displays minimize maximize keep awake", labels: &["Always-visible applets", "Default apps", "Always visible"], target: "", subpage: "" },
    Setting { section: "dock", key: "tray", title: "System tray", detail: "Choose which running tray applications to always show to the left of the always-visible applets.", keywords: "system tray always show app icons collapse applets", labels: &["System tray", "Always show"], target: "", subpage: "" },
    Setting { section: "datetime", key: "automatic", title: "Automatic date & time", detail: "Synchronize the system clock online.", keywords: "date time automatic ntp clock sync network", labels: &["Automatic date & time"], target: "ntp_switch", subpage: "" },
    Setting { section: "datetime", key: "timezone", title: "Time zone", detail: "Choose the local time zone when automatic date and time is off.", keywords: "date time timezone zone city region local", labels: &["Time zone"], target: "timezone_entry", subpage: "" },
    Setting { section: "datetime", key: "time", title: "Time (24-hour)", detail: "Set the system time manually.", keywords: "date time manual hours minutes seconds clock", labels: &["Time (24-hour)"], target: "hour_spin", subpage: "" },
    Setting { section: "datetime", key: "date", title: "System date", detail: "Set the system date manually.", keywords: "date time calendar manual day month year", labels: &["System date"], target: "", subpage: "" },
    Setting { section: "datetime", key: "stack", title: "Stack time and date", detail: "Display the clock above the date or in one horizontal line.", keywords: "date time clock stacked horizontal vertical layout", labels: &["Stack time and date"], target: "clock_stack_switch", subpage: "" },
    Setting { section: "datetime", key: "show_date", title: "Show date", detail: "Show or hide the date in the dock clock.", keywords: "date time clock show hide date", labels: &["Show date"], target: "clock_show_date_switch", subpage: "" },
    Setting { section: "datetime", key: "show_year", title: "Show year", detail: "Show or hide the year in the dock date.", keywords: "date time clock show hide year", labels: &["Show year"], target: "clock_show_year_switch", subpage: "" },
    Setting { section: "datetime", key: "weekday", title: "Show day of the week", detail: "Show the weekday before the time and date.", keywords: "date time clock weekday day week monday sunday", labels: &["Show day of the week"], target: "clock_show_weekday_switch", subpage: "" },
    Setting { section: "datetime", key: "reverse", title: "Date before time", detail: "Reverse the order of the dock clock's date and time.", keywords: "date time clock reverse order position", labels: &["Date before time"], target: "clock_reverse_switch", subpage: "" },
    Setting { section: "datetime", key: "format", title: "Time format", detail: "Choose a 12-hour or 24-hour dock clock.", keywords: "date time clock format 12 24 hour am pm military", labels: &["Time format"], target: "clock_hour_combo", subpage: "" },
    Setting { section: "datetime", key: "order", title: "Date order", detail: "Choose the order of month, day, and year.", keywords: "date time clock format month day year mdy dmy ymd", labels: &["Date order"], target: "clock_date_combo", subpage: "" },
    Setting { section: "datetime", key: "separator", title: "Date separator", detail: "Choose slash, dash, or dot separators for the date.", keywords: "date time clock format separator slash dash dot", labels: &["Date separator"], target: "clock_separator_combo", subpage: "" },
    Setting { section: "language", key: "language", title: "Language", detail: "Choose the language used by the system and desktop suite.", keywords: "language locale translation regional english spanish french german install", labels: &["Choose your language", "Language"], target: "", subpage: "" },
    Setting { section: "power", key: "dim_before_lock", title: "Dim screen before locking", detail: "Fade the screen out shortly before it locks.", keywords: "power dim fade screen lock idle warning", labels: &["Dim screen before locking"], target: "", subpage: "" },
    Setting { section: "power", key: "lid_battery", title: "Lid closed on battery", detail: "Choose what happens when the lid is closed on battery power.", keywords: "power lid laptop close closed battery suspend sleep hibernate nothing", labels: &["Lid closed on battery"], target: "", subpage: "" },
    Setting { section: "power", key: "lid_plugged", title: "Lid closed when plugged in", detail: "Choose what happens when the lid is closed while plugged in.", keywords: "power lid laptop close closed plugged ac charger suspend sleep nothing", labels: &["Lid closed when plugged in"], target: "", subpage: "" },
    Setting { section: "power", key: "power_button", title: "Power button", detail: "Choose what the power button does.", keywords: "power button key shutdown turn off suspend sleep hibernate menu", labels: &["Power button"], target: "", subpage: "" },
    Setting { section: "power", key: "low_battery", title: "Low battery", detail: "Choose when to warn that the battery is low.", keywords: "power battery low warning notification percent level", labels: &["Low battery"], target: "", subpage: "" },
    Setting { section: "power", key: "critical_battery", title: "Critical battery action", detail: "Choose what happens when the battery is almost empty.", keywords: "power battery critical warning notification hibernate shutdown suspend empty", labels: &["Critical battery action"], target: "", subpage: "" },
    Setting { section: "audio", key: "mic_meter", title: "Microphone level meter", detail: "See the input level while you speak to test a microphone.", keywords: "sound audio input level mic microphone peak meter test volume", labels: &["Microphone level meter"], target: "", subpage: "inputs" },
    Setting { section: "audio", key: "tray_switch", title: "Switch output/input device from tray", detail: "Change the default output or input device from the tray menu.", keywords: "sound audio default output input device speakers headset headphones microphone dropdown tray switch", labels: &["Switch output/input device from tray"], target: "", subpage: "" },
    Setting { section: "network", key: "captive_portal", title: "Hotspot sign-in", detail: "Get a sign-in notification when a Wi-Fi network needs a captive portal login.", keywords: "network captive portal hotspot sign in login notification wifi public hotel airport", labels: &["Hotspot sign-in", "Captive portal", "Sign-in notification"], target: "", subpage: "" },
    Setting { section: "datetime", key: "first_weekday", title: "First day of the week", detail: "Choose the day that starts the week in calendars.", keywords: "date time calendar first day of week week start monday sunday saturday", labels: &["First day of the week"], target: "", subpage: "" },
    Setting { section: "datetime", key: "calendars", title: "Calendars", detail: "Subscribe to calendars and show their events.", keywords: "date time calendars ics ical events subscribe webcal online calendar", labels: &["Calendars"], target: "", subpage: "" },
    Setting { section: "behavior", key: "tiling", title: "Window tiling", detail: "Arrange windows automatically in tiles.", keywords: "desktop behavior tiling tile tiled windows automatic layout dwindle scrolling omarchy", labels: &["Window tiling"], target: "tiling_switch", subpage: "" },
    Setting { section: "behavior", key: "focus_outline", title: "Focused window outline", detail: "Draw an outline around the focused window.", keywords: "desktop behavior focus focused window outline border highlight active", labels: &["Focused window outline"], target: "focus_outline_switch", subpage: "" },
    Setting { section: "behavior", key: "authentication", title: "Authentication", detail: "View the status of the administrator password prompt.", keywords: "desktop authentication polkit password prompt administrator agent", labels: &["Authentication"], target: "authentication_status", subpage: "" },
    Setting { section: "theme", key: "style", title: "Style (light/dark)", detail: "Choose a light or dark appearance.", keywords: "dark light mode color colour scheme theme style appearance", labels: &["Style (light/dark)"], target: "", subpage: "" },
    Setting { section: "layout", key: "previous", title: "Restore previous layout", detail: "Return to the layout used before the last change.", keywords: "layout preset restore previous undo revert", labels: &["Restore previous layout"], target: "previous", subpage: "" },
    Setting { section: "keepawake", key: "duration", title: "Keep awake duration", detail: "Choose how long the session stays awake.", keywords: "inhibit idle awake duration time minutes hours until", labels: &["Duration"], target: "", subpage: "" },
    Setting { section: "lock", key: "battery", title: "Show battery level", detail: "Show the battery level on the lock screen.", keywords: "lock screen battery level charge percent", labels: &["Show battery level"], target: "battery", subpage: "" },
    Setting { section: "lock", key: "notifications", title: "Show new notification count", detail: "Show how many notifications arrived while the screen was locked.", keywords: "lock screen notifications count new unread", labels: &["Show new notification count"], target: "notifications", subpage: "" },
    Setting { section: "lock", key: "notification_names", title: "Show application names", detail: "Show which applications sent notifications while locked.", keywords: "lock screen notifications application names privacy", labels: &["Show application names"], target: "notification_names", subpage: "" },
    Setting { section: "capture", key: "copy_to_clipboard", title: "Copy screenshots to clipboard automatically", detail: "Every new screenshot is also placed on the clipboard as an image.", keywords: "screenshot capture clipboard copy paste image automatic", labels: &["Copy screenshots to clipboard automatically"], target: "", subpage: "" },
    Setting { section: "capture", key: "open_editor", title: "Open the editor after taking a screenshot", detail: "Annotate or crop each screenshot before sharing it. The notification Edit button always opens it.", keywords: "screenshot capture editor annotate edit crop draw", labels: &["Open the editor after taking a screenshot"], target: "", subpage: "" },
    Setting { section: "weather", key: "location", title: "Location", detail: "Choose the location used for weather reports.", keywords: "weather location city town zip postal latitude longitude forecast", labels: &["Location"], target: "", subpage: "" },
    Setting { section: "weather", key: "current_location", title: "Use current location", detail: "Use system location services to choose your weather location.", keywords: "weather current location automatic geolocation gps geoclue location services", labels: &["Use current location"], target: "", subpage: "" },
    Setting { section: "weather", key: "units", title: "Units", detail: "Choose temperature and measurement units.", keywords: "weather units celsius fahrenheit temperature metric imperial degrees", labels: &["Units", "Temperature units"], target: "", subpage: "" },
    Setting { section: "weather", key: "forecast", title: "Forecast", detail: "View the weather forecast for the selected location.", keywords: "weather forecast rain snow wind humidity conditions", labels: &["Forecast"], target: "", subpage: "" },
    Setting { section: "shortcuts", key: "shortcuts-list", title: "Keyboard shortcuts", detail: "View every shortcut for windows, workspaces, tiling, launchers, media and the system.", keywords: "keyboard shortcuts hotkeys keybindings keybinds keys bindings list super", labels: &["Keyboard Shortcuts"], target: "", subpage: "" },
    Setting { section: "shortcuts", key: "shortcuts-search", title: "Change a shortcut", detail: "Assign a different key combination to one of your shortcuts.", keywords: "keyboard shortcut rebind remap change hotkey keybinding conflict", labels: &["Search shortcuts"], target: "", subpage: "" },
    Setting { section: "shortcuts", key: "shortcuts-overview", title: "Shortcut overview", detail: "Press Super + / to show every shortcut at any time.", keywords: "keyboard shortcuts cheat sheet overview help super slash", labels: &["Keyboard Shortcuts"], target: "search", subpage: "" },
    Setting { section: "about", key: "about-info", title: "System information", detail: "View the operating system, kernel, processor, memory, graphics and disk.", keywords: "about system information os version kernel cpu processor memory ram gpu graphics disk storage labwc hardware", labels: &["Operating system"], target: "", subpage: "" },
    Setting { section: "about", key: "about-copy", title: "Copy system information", detail: "Copy the system details to the clipboard for support requests.", keywords: "about copy clipboard system information bug report support", labels: &["Copy to Clipboard"], target: "", subpage: "" },
    Setting { section: "about", key: "about-restart", title: "Restart desktop shell", detail: "Restart the dock, desktop shell and notifications without closing applications.", keywords: "restart reload desktop shell dock panel notifications frozen stuck", labels: &["Restart Desktop Shell…"], target: "", subpage: "" },
    Setting { section: "users", key: "users-name", title: "Your name", detail: "Change the full name shown for your account.", keywords: "user account full real name display name", labels: &["Full name"], target: "", subpage: "" },
    Setting { section: "users", key: "users-picture", title: "Account picture", detail: "Choose the picture shown for your account.", keywords: "user account avatar picture photo face icon", labels: &["Change picture"], target: "", subpage: "" },
    Setting { section: "users", key: "users-password", title: "Change password", detail: "Change your account password.", keywords: "user account password change login security", labels: &["Password", "Change Password…"], target: "", subpage: "" },
    Setting { section: "users", key: "users-others", title: "Other users", detail: "View the other accounts on this computer.", keywords: "users accounts administrator list people", labels: &["Other users"], target: "", subpage: "" },
    Setting { section: "defaultapps", key: "default-browser", title: "Web browser", detail: "Choose the application that opens web links.", keywords: "default apps applications browser web internet firefox chrome links http", labels: &["Web browser"], target: "", subpage: "" },
    Setting { section: "defaultapps", key: "default-mail", title: "Mail", detail: "Choose the application that opens email links.", keywords: "default apps applications mail email mailto client thunderbird", labels: &["Mail"], target: "", subpage: "" },
    Setting { section: "defaultapps", key: "default-files", title: "File manager", detail: "Choose the application that opens folders.", keywords: "default apps applications file manager folders directory files", labels: &["File manager"], target: "", subpage: "" },
    Setting { section: "defaultapps", key: "default-terminal", title: "Terminal", detail: "Choose the terminal used by the desktop and shortcuts.", keywords: "default apps applications terminal console shell command line", labels: &["Terminal"], target: "", subpage: "" },
    Setting { section: "defaultapps", key: "default-editor", title: "Text editor", detail: "Choose the application that opens text files.", keywords: "default apps applications text editor plain notes", labels: &["Text editor"], target: "", subpage: "" },
    Setting { section: "defaultapps", key: "default-images", title: "Image viewer", detail: "Choose the application that opens pictures.", keywords: "default apps applications image viewer photos pictures png jpeg", labels: &["Image viewer"], target: "", subpage: "" },
    Setting { section: "defaultapps", key: "default-video", title: "Video player", detail: "Choose the application that plays videos.", keywords: "default apps applications video player movies mp4 mkv", labels: &["Video player"], target: "", subpage: "" },
    Setting { section: "defaultapps", key: "default-music", title: "Music player", detail: "Choose the application that plays music.", keywords: "default apps applications music audio player songs mp3 flac", labels: &["Music player"], target: "", subpage: "" },
    Setting { section: "accessibility", key: "a11y-text", title: "Text size", detail: "Make text in applications larger.", keywords: "accessibility a11y text size scaling font larger bigger zoom", labels: &["Text size"], target: "", subpage: "" },
    Setting { section: "accessibility", key: "a11y-contrast", title: "High contrast", detail: "Increase the contrast of controls and text.", keywords: "accessibility a11y high contrast visibility", labels: &["High contrast"], target: "", subpage: "" },
    Setting { section: "accessibility", key: "a11y-motion", title: "Reduce motion", detail: "Turn off animations.", keywords: "accessibility a11y reduce reduced motion animations effects", labels: &["Reduce motion"], target: "", subpage: "" },
    Setting { section: "accessibility", key: "a11y-cursor-size", title: "Cursor size", detail: "Choose the size of the mouse pointer.", keywords: "accessibility a11y cursor pointer mouse size", labels: &["Cursor size"], target: "", subpage: "" },
    Setting { section: "accessibility", key: "a11y-large-cursor", title: "Large cursor", detail: "Use a pointer that is easier to find.", keywords: "accessibility a11y large big cursor pointer mouse", labels: &["Large cursor"], target: "", subpage: "" },
    Setting { section: "accessibility", key: "a11y-screen-reader", title: "Screen reader", detail: "Read the screen aloud with Orca.", keywords: "accessibility a11y screen reader orca speech blind narrator", labels: &["Screen reader"], target: "", subpage: "" },
    Setting { section: "nightlight", key: "night-enabled", title: "Night light", detail: "Use warmer screen colors in the evening.", keywords: "night light blue light filter warm color temperature evening redshift wlsunset", labels: &["Night light"], target: "", subpage: "" },
    Setting { section: "nightlight", key: "night-schedule", title: "Night light schedule", detail: "Follow sunset and sunrise, or choose times.", keywords: "night light schedule sunset sunrise times automatic manual", labels: &["Schedule"], target: "", subpage: "" },
    Setting { section: "nightlight", key: "night-temperature", title: "Night light color temperature", detail: "Choose how warm the screen looks at night.", keywords: "night light color colour temperature warm kelvin", labels: &["Color temperature"], target: "", subpage: "" },
    Setting { section: "nightlight", key: "night-times", title: "Night light times", detail: "Choose when night light starts and ends.", keywords: "night light from to start end time manual", labels: &["Times"], target: "", subpage: "" },
    Setting { section: "weather", key: "refresh", title: "Refresh", detail: "Update weather information.", keywords: "weather refresh update reload interval", labels: &["Refresh", "Refresh interval"], target: "", subpage: "" },
];

/// One section of the sidebar: (key, English name, icon, module group).
pub type Section = (&'static str, &'static str, &'static str, &'static str);

pub struct SearchIndex {
    /// key → (translated name, icon)
    pub sections: HashMap<&'static str, (String, &'static str)>,
    pub items: Vec<Setting>,
    /// Owned section-entry details ("Open {section} settings.").
    details: Vec<String>,
    documents: Vec<(usize, String, String, String)>,
}

impl SearchIndex {
    pub fn new(sections: &[Section], translate: &dyn Fn(&str) -> String) -> SearchIndex {
        let mut names = HashMap::new();
        for (key, name, icon, _module) in sections {
            names.insert(*key, (translate(name), *icon));
        }
        let mut items = Vec::new();
        let mut details = Vec::new();
        for (key, name, _icon, _module) in sections {
            // Python: "Open {section} settings.".format(section=translate(name)) — not translated itself.
            details.push(format!("Open {} settings.", translate(name)));
            items.push(Setting::new(key, "__section__", name, ""));
        }
        items.extend(SETTINGS.iter().filter(|item| names.contains_key(item.section)).cloned());
        let mut documents = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let detail_source: &str = if index < details.len() { &details[index] } else { item.detail };
            let title = normalize(&translate(item.title));
            let english = normalize(item.title);
            let section = &names[item.section].0;
            let terms = [title.clone(), english.clone(), normalize(section), normalize(item.section),
                         normalize(&translate(detail_source)), normalize(detail_source), normalize(item.keywords)].join(" ");
            documents.push((index, title, english, terms));
        }
        SearchIndex { sections: names, items, details, documents }
    }

    /// The (untranslated) detail text of an item, as Python stored it.
    pub fn detail(&self, index: usize) -> &str {
        if index < self.details.len() { &self.details[index] } else { self.items[index].detail }
    }

    /// Matching item indices, best first.
    pub fn search_indices(&self, query: &str) -> Vec<usize> {
        let query = normalize(query);
        if query.is_empty() {
            return Vec::new();
        }
        let words: Vec<&str> = query.split(' ').collect();
        let mut matches: Vec<((u8, String, String, &'static str), usize)> = Vec::new();
        for (index, title, english, terms) in &self.documents {
            if !words.iter().all(|word| terms.contains(word)) {
                continue;
            }
            let names = [title.as_str(), english.as_str()];
            let token_prefixes = |text: &str| words.iter().all(|word| text.split_whitespace().any(|t| t.starts_with(word)));
            let rank = if names.contains(&query.as_str()) {
                0
            } else if names.iter().any(|n| n.starts_with(&query)) {
                1
            } else if names.iter().any(|n| token_prefixes(n)) {
                2
            } else if names.iter().any(|n| words.iter().all(|w| n.contains(w))) {
                3
            } else if token_prefixes(terms) {
                4
            } else {
                5
            };
            let item = &self.items[*index];
            matches.push(((rank, title.clone(), suite_core::pyjson::casefold(&self.sections[item.section].0), item.key), *index));
        }
        matches.sort_by(|a, b| a.0.cmp(&b.0));
        matches.into_iter().map(|(_, index)| index).collect()
    }

    pub fn search(&self, query: &str) -> Vec<&Setting> {
        self.search_indices(query).into_iter().map(|i| &self.items[i]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::SECTIONS;
    use std::collections::HashSet;

    fn plain(text: &str) -> String { text.to_owned() }

    fn keys(index: &SearchIndex, query: &str) -> Vec<(&'static str, &'static str)> {
        index.search(query).iter().map(|s| (s.section, s.key)).collect()
    }

    #[test]
    fn every_registered_section_has_metadata_and_keys_are_unique() {
        let sections: HashSet<_> = SECTIONS.iter().map(|s| s.0).collect();
        assert_eq!(SETTINGS.iter().map(|s| s.section).collect::<HashSet<_>>(), sections);
        assert_eq!(SETTINGS.len(), SETTINGS.iter().map(|s| (s.section, s.key)).collect::<HashSet<_>>().len());
        assert!(SETTINGS.iter().all(|s| !s.title.is_empty() && !s.detail.is_empty()));
    }

    #[test]
    fn exact_control_label_ranks_before_synonyms_and_case_is_ignored() {
        let index = SearchIndex::new(SECTIONS, &plain);
        for query in ["Maximum charge", "MAXIMUM CHARGE", "  maximum   Charge  "] {
            assert_eq!(keys(&index, query)[0], ("battery", "charge"));
        }
        assert_eq!(index.search("Time format")[0].key, "format");
        assert_eq!(index.search("Audio")[0].key, "__section__");
    }

    #[test]
    fn common_synonyms_and_multiple_words_find_real_controls() {
        let index = SearchIndex::new(SECTIONS, &plain);
        for (query, section, key) in [
            ("microphone", "audio", "inputs"), ("volume", "audio", "outputs"),
            ("dns", "network", "saved"), ("hibernate", "power", "sleep_mode"),
            ("opacity app", "theme", "app_transparency"), ("hot corner", "behavior", "hot_corners"),
            ("24 hour", "datetime", "time"), ("timezone", "datetime", "timezone"),
            ("fahrenheit", "weather", "units"), ("wifi", "network", "wifi"),
            ("lock screen weather", "lock", "weather"), ("lock screen forecast", "lock", "forecast"),
            ("pause media", "lock", "pause_media"), ("lock screen media controls", "lock", "media"),
        ] {
            assert!(keys(&index, query).contains(&(section, key)), "{query}");
        }
        assert!(index.search("not_a_real_setting_zyx").is_empty());
        assert!(index.search("").is_empty());
    }

    #[test]
    fn translated_titles_are_searchable_alongside_english() {
        let translate = |text: &str| match text {
            "Brightness" => "Luminosité".to_owned(),
            "Displays" => "Écrans".to_owned(),
            "Show date" => "Afficher la date".to_owned(),
            other => other.to_owned(),
        };
        let index = SearchIndex::new(SECTIONS, &translate);
        assert_eq!(index.search("LUMINOSITE")[0].key, "brightness");
        assert_eq!(index.search("brightness")[0].key, "brightness");
        assert_eq!(index.search("afficher la date")[0].key, "show_date");
        assert_eq!(normalize("Wi-Fi"), normalize("wi fi"));
    }

    #[test]
    fn unregistered_pages_are_not_offered_and_search_is_deterministic() {
        let sections: Vec<Section> = SECTIONS.iter().copied().filter(|s| s.0 != "weather").collect();
        let index = SearchIndex::new(&sections, &plain);
        assert!(!index.search("weather").iter().any(|s| s.section == "weather"));
        let full = SearchIndex::new(SECTIONS, &plain);
        assert_eq!(full.search("display"), full.search("display"));
    }
}
