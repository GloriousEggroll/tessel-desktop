#!/bin/sh
# Run a command inside a private nested headless labwc (tools/headless.md).
# Never touches the live session: own XDG_RUNTIME_DIR, own D-Bus, no input devices.
# Usage: headless.sh <command> [args...]   (exit status = the command's)
# Workspaces: the private rc.xml declares 3 desktops so ext-workspace has content.
set -eu
AGENT=${AGENT:-wayland}
R=/run/user/1000/cbt/rust-$AGENT/n
mkdir -p "$R/rt" "$R/cfg"; chmod 700 "$R/rt"
sed "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" /usr/share/dbus-1/session.conf > "$R/d.conf"
cat > "$R/cfg/rc.xml" <<'XML'
<?xml version="1.0"?>
<labwc_config>
  <desktops number="3"><popupTime>0</popupTime></desktops>
</labwc_config>
XML
: > "$R/cfg/autostart"
rm -f "$R/result" "$R/log"
{
  echo '#!/bin/sh'
  echo 'case "$XDG_RUNTIME_DIR:$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac'
  printf 'SUITE_WAYLAND_HEADLESS=1 GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo timeout %s' "${TIMEOUT:-180}"
  for arg in "$@"; do printf " '%s'" "$(printf %s "$arg" | sed "s/'/'\\\\''/g")"; done
  printf ' > "%s/log" 2>&1; echo $? > "%s/result"\n' "$R" "$R"
  echo 'labwc --exit'
} > "$R/run.sh"
chmod +x "$R/run.sh"
env -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 RUST_BACKTRACE=1 SUITE_WAYLAND_C_HELPERS="${SUITE_WAYLAND_C_HELPERS:-}" \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
cat "$R/log" 2>/dev/null || true
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
