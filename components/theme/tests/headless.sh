#!/bin/sh
# Run one command inside a private nested headless labwc (tools/headless.md):
# own XDG_RUNTIME_DIR, own D-Bus session, private XDG dirs, no input devices.
# Never touches the live session. Usage: headless.sh <outputs> <command...>
set -eu
R=/run/user/1000/cbt/rust-${AGENT:-ws-prompt}/n
OUTPUTS=$1; shift
mkdir -p "$R/rt" "$R/cfg" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data"; chmod 700 "$R/rt"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
rm -f "$R/result"
{
  echo '#!/bin/sh'
  echo 'case "$XDG_RUNTIME_DIR:$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac'
  echo "export TESSEL_SHELL_UI_ISOLATED=1 GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GSETTINGS_BACKEND=memory"
  echo "export UI_TEST_FILTER='${UI_TEST_FILTER:-}'"
  echo "export XDG_CONFIG_HOME='$R/xdg/config' XDG_CACHE_HOME='$R/xdg/cache' XDG_DATA_HOME='$R/xdg/data'"
  printf 'timeout 300'; for a in "$@"; do printf " '%s'" "$a"; done; printf ' > "%s/log" 2>&1; echo $? > "%s/result"\n' "$R" "$R"
  echo 'labwc --exit'
} > "$R/run.sh"
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS="$OUTPUTS" \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
cat "$R/log"
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
