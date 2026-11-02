#!/bin/sh
# Python vs Rust tessel-dock in a private nested headless labwc (tools/headless.md):
# time to READY (sd_notify on a private NOTIFY_SOCKET), idle RSS/PSS, app-menu
# open latency, and screenshots (bar, dock, app menu, popups) compared pixelwise.
# Each implementation gets a fresh fake HOME (config, caches, fake status
# scripts) under the private runtime; never touches the live session.
# Usage: run.sh [scenario ...]   (default: dock bar custombar customleft customdock;
# the custom* scenarios use custom group_layouts, see SCENARIO_CONFIG in driver.py)
set -eu
R=/run/user/1000/cbt/rust-tessel-dock/cmp
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/out"; chmod 700 "$R/rt"
mkdir -p "$R/svc"; sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "s|<standard_session_servicedirs */>|<servicedir>$R/svc</servicedir>|" /usr/share/dbus-1/session.conf > "$R/d.conf"
printf '<labwc_config><mouse><default/><context name="Root"><mousebind button="Left" action="Press"/><mousebind button="Right" action="Press"/><mousebind button="Middle" action="Press"/></context></mouse></labwc_config>\n' > "$R/cfg/rc.xml"
printf '<openbox_menu><menu id="root-menu" label=""><item label="-"/></menu></openbox_menu>\n' > "$R/cfg/menu.xml"
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
if [ -z "${RUST_BIN:-}" ]; then
  (cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-tessel-dock cargo build -q --release -p tessel-dock --bin tessel-dock)
  RUST_BIN="$ROOT/target/release/tessel-dock"
fi
SCEN=${*:-dock bar custombar customleft customdock}
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" GSETTINGS_BACKEND=memory NO_AT_BRIDGE=1 GTK_A11Y=none GDK_BACKEND=wayland GSK_RENDERER=${GSK_RENDERER:-cairo}
timeout 600 python3 -I "$HERE/driver.py" "$R" "$RUST_BIN" $SCEN > "$R/driver.log" 2>&1
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" LANG="${LANG:-C.UTF-8}" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless \
  WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
cat "$R/driver.log"
