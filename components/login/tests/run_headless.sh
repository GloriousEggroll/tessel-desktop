#!/bin/sh
# Run tests/ui_headless.rs inside a private nested headless labwc (tools/headless.md)
# with two outputs. Never touches the live session: own XDG_RUNTIME_DIR and Wayland
# socket, own D-Bus (also used as the "system" bus; no service directories, so
# nothing can be bus-activated), private HOME and XDG dirs. greetd is only ever a
# FakeClient or a fake socket server inside the test; GREETD_SOCK points at a
# private path that does not exist. logind is a fake object on the private bus.
# wtype/wlrctl/grim connect to the private compositor only.
# Extra arguments filter check names.
set -eu
AGENT=${AGENT:-login}
S=/run/user/1000/cbt/rust-$AGENT
R=$S/n
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/tmp" "$R/home" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data" "$R/xdg/state"; chmod 700 "$R/rt"
printf '<labwc_config/>\n' > "$R/cfg/rc.xml"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs, logind or other service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
BIN=$(cd "$ROOT" && env -u TMPDIR cargo test -p tessel-login --test ui_headless --no-run --message-format=json 2>/dev/null \
      | jq -r 'select(.reason == "compiler-artifact" and .target.name == "ui_headless" and .executable != null) | .executable')
TESSEL_KEYBOARD=$(ls -t "$ROOT"/target/debug/build/tessel-keyboard-*/out/tessel-keyboard-input | head -1)
rm -f "$R/result"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
LANG=C.UTF-8 HOME="$R/home" DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" LOGIN_UI_ISOLATED_TEST=1 GREETD_SOCK="$R/never-greetd.sock" \
TESSEL_KEYBOARD_INPUT="$TESSEL_KEYBOARD" GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GSETTINGS_BACKEND=memory \
GTK_USE_PORTAL=0 GDK_DEBUG=no-portals GIO_USE_VFS=local TMPDIR="$R/tmp" XDG_CONFIG_HOME="$R/xdg/config" \
XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data" XDG_STATE_HOME="$R/xdg/state" XDG_CONFIG_DIRS="$R/none" \
XDG_DATA_DIRS="$R/xdg/data:/usr/share" timeout 600 "$BIN" $* > "$R/log" 2>&1; echo \$? > "$R/result"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -u WAYLAND_DISPLAY -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS -u TMPDIR -i HOME="$R/home" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" \
  TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=2 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
cat "$R/log"
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
