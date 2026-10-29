#!/bin/sh
# Run one command inside a private nested headless labwc (tools/headless.md).
#
#   AGENT=gs-x components/global-settings/tests/headless.sh <command> [args…]
#
# Isolation: own XDG_RUNTIME_DIR ($R/rt) and Wayland socket, own dbus-daemon
# (also used as the "system" bus) WITHOUT <standard_session_servicedirs/>, so
# nothing real can be bus-activated; fake HOME and XDG dirs under $R; no input
# devices; GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0. $R/bin is
# first on PATH and holds logging fakes for every privileged / session-changing
# tool (pkexec, sudo, systemctl, labwc, localectl, passwd, setsid, dnf, flatpak,
# wlrctl, notify-send, xrandr, wlr-randr, busctl, loginctl, gsettings, nmcli,
# bluetoothctl, rfkill). Calls are appended to $R/tmp/calls.log; a test may
# replace a fake by writing its own script into $GS_FAKE_BIN before use.
# Exports for the command: GS_HEADLESS=1 GS_ROOT=$R GS_FAKE_BIN=$R/bin
# BTMANAGER=$HOME_REAL/btmanager (read-only Python sources).
# The command's exit status is returned; its output is in $R/log (also printed).
# Setting KEEP=1 keeps $R from a previous run (default: fresh).
set -eu
AGENT=${AGENT:-gs}
R=/run/user/1000/cbt/rust-$AGENT/n
[ "${KEEP:-0}" = 1 ] || rm -rf "$R"
mkdir -p "$R/rt" "$R/cfg" "$R/tmp" "$R/bin" "$R/home/.config/labwc" "$R/home/.cache" "$R/home/.local/share" \
  "$R/home/.local/state" "$R/home/Pictures"
chmod 700 "$R/rt"
[ -f "$R/cfg/rc.xml" ] || printf '<?xml version="1.0"?>\n<labwc_config/>\n' > "$R/cfg/rc.xml"
: > "$R/cfg/autostart"
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" \
  /usr/share/dbus-1/session.conf > "$R/d.conf"
for tool in pkexec sudo systemctl labwc localectl passwd setsid dnf flatpak wlrctl notify-send xrandr wlr-randr \
            busctl loginctl gsettings nmcli bluetoothctl rfkill chsh usermod; do
  [ -e "$R/bin/$tool" ] && continue
  printf '#!/bin/sh\nprintf "%%s\\n" "%s $*" >> "%s/tmp/calls.log"\nexit ${GS_FAKE_STATUS:-0}\n' "$tool" "$R" > "$R/bin/$tool"
done
chmod +x "$R/bin/"*
REAL_HOME=$HOME
rm -f "$R/result" "$R/log"
{
  echo '#!/bin/sh'
  echo 'case "$XDG_RUNTIME_DIR:$WAYLAND_DISPLAY" in /run/user/1000:*|*:) /usr/bin/labwc --exit; exit 1;; esac'
  printf 'export GS_HEADLESS=1 GS_ROOT=%s GS_FAKE_BIN=%s/bin BTMANAGER=%s/btmanager\n' "'$R'" "'$R'" "'$REAL_HOME'"
  printf 'export PATH=%s/bin:"$PATH" HOME=%s/home DBUS_SYSTEM_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS"\n' "'$R'" "'$R'"
  printf 'export XDG_CONFIG_HOME=%s/home/.config XDG_CACHE_HOME=%s/home/.cache XDG_DATA_HOME=%s/home/.local/share XDG_STATE_HOME=%s/home/.local/state\n' "'$R'" "'$R'" "'$R'" "'$R'"
  echo 'export XDG_CONFIG_DIRS=/nonexistent GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GSETTINGS_BACKEND=memory'
  echo 'export GTK_USE_PORTAL=0 GDK_DEBUG=no-portals GIO_USE_VFS=local LANG=C.UTF-8 PYTHONDONTWRITEBYTECODE=1'
  printf 'export TMPDIR=%s/tmp PIPEWIRE_REMOTE=%s/no-pipewire PULSE_SERVER=unix:%s/no-pulse\n' "'$R'" "'$R'" "'$R'"
  printf 'timeout %s' "${TIMEOUT:-600}"
  for a in "$@"; do printf " '%s'" "$(printf '%s' "$a" | sed "s/'/'\\\\''/g")"; done
  printf ' > %s/log 2>&1; echo $? > %s/result\n' "'$R'" "'$R'"
  echo '/usr/bin/labwc --exit'
} > "$R/run.sh"
chmod +x "$R/run.sh"
env -u WAYLAND_DISPLAY -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS -u TMPDIR -i HOME="$R/home" PATH="$PATH" LANG=C.UTF-8 \
  XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS="${OUTPUTS:-1}" WLR_RENDERER=pixman \
  WLR_LIBINPUT_NO_DEVICES=1 RUST_BACKTRACE=1 \
  dbus-run-session --config-file="$R/d.conf" -- /usr/bin/labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
cat "$R/log" 2>/dev/null || true
if grep -q "Activating service" "$R/labwc.log"; then
  grep "Activating service" "$R/labwc.log"; echo "UNEXPECTED D-Bus activation on the private bus"; exit 1
fi
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
