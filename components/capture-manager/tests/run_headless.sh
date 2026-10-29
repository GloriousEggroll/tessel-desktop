#!/bin/sh
# Run tests/ui_headless.rs inside a private nested headless labwc (tools/headless.md)
# with two outputs. Never touches the live session: own XDG_RUNTIME_DIR and Wayland
# socket, own D-Bus (also used as the "system" bus), private HOME and XDG dirs
# (config.json, shortcuts.json, rc.xml, mimeapps.list, Pictures/Videos all stay in
# $R), no input devices. The ScreenCast portal, notification server, Waydock and
# FileManager1 are fakes inside the test; pactl, wpctl, wl-copy and labwc are
# logging fakes first on PATH; ffmpeg is wrapped to refuse any PulseAudio input;
# PIPEWIRE_REMOTE/PULSE_SERVER point at nonexistent private paths.
# Extra arguments filter check names.
set -eu
AGENT=${AGENT:-capture-manager}
R=/run/user/1000/cbt/rust-$AGENT/n
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/tmp" "$R/bin" "$R/home/Pictures" "$R/home/Videos" \
  "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data" "$R/xdg/state"; chmod 700 "$R/rt"
printf '<labwc_config/>\n' > "$R/cfg/rc.xml"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
printf 'XDG_PICTURES_DIR="%s/home/Pictures"\nXDG_VIDEOS_DIR="%s/home/Videos"\n' "$R" "$R" > "$R/xdg/config/user-dirs.dirs"
cat > "$R/bin/pactl" <<FAKE
#!/bin/sh
echo "pactl \$*" >> "$R/tmp/calls.log"
case "\$*" in
  "--format=json list sources") echo '[{"name":"speaker.monitor","description":"Private speaker"},{"name":"mic","description":"Private microphone"}]';;
  get-default-sink) echo speaker;;
  get-default-source) echo mic;;
  *) exit 1;;
esac
FAKE
printf '#!/bin/sh\necho "wpctl $*" >> "%s/tmp/calls.log"\n' "$R" > "$R/bin/wpctl"
printf '#!/bin/sh\necho "labwc $*" >> "%s/tmp/calls.log"\n' "$R" > "$R/bin/labwc"
cat > "$R/bin/wl-copy" <<FAKE
#!/bin/sh
cat > "$R/tmp/wl-copy.last"
echo "\$* \$(wc -c < "$R/tmp/wl-copy.last")" >> "$R/tmp/wl-copy.log"
FAKE
cat > "$R/bin/ffmpeg" <<FAKE
#!/bin/sh
for a in "\$@"; do [ "\$a" = pulse ] && { echo "refused ffmpeg \$*" >> "$R/tmp/calls.log"; exit 1; }; done
exec /usr/bin/ffmpeg "\$@"
FAKE
chmod +x "$R/bin/"*
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
BIN=$(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo test -p capture-manager --test ui_headless --no-run --message-format=json 2>/dev/null \
      | python3 -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="ui_headless" and m.get("executable"): print(m["executable"])')
rm -f "$R/result"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
PATH="$R/bin:\$PATH" HOME="$R/home" DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" CAPTURE_UI_ISOLATED_TEST=1 \
PIPEWIRE_REMOTE="$R/no-pipewire" PULSE_SERVER="unix:$R/no-pulse" GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none \
GSK_RENDERER=cairo GSETTINGS_BACKEND=memory GTK_USE_PORTAL=0 GDK_DEBUG=no-portals GIO_USE_VFS=local TMPDIR="$R/tmp" XDG_CONFIG_HOME="$R/xdg/config" \
XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data" XDG_STATE_HOME="$R/xdg/state" XDG_CONFIG_DIRS="$R/none" XDG_DATA_DIRS="$R/xdg/data:/usr/share" \
timeout 600 "$BIN" $* > "$R/log" 2>&1; echo \$? > "$R/result"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -u WAYLAND_DISPLAY -u DISPLAY -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless \
  WLR_HEADLESS_OUTPUTS=2 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
cat "$R/log"
grep -h "refused" "$R/tmp/calls.log" 2>/dev/null && echo "ffmpeg PulseAudio access was refused (see above)"
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
