#!/bin/sh
# Real-configuration memory harness: Python vs Rust tessel-dock + tessel-shell in a
# private headless labwc (tools/headless.md) with two 3840x2160 outputs, the
# live session's renderer (labwc WLR_RENDERER=vulkan on the GPU render node,
# GTK's default GSK renderer) and a fake HOME made of copies of the real config
# (make_home.sh). Never touches the live session or the real ~/.config/~/.local.
# Usage: run.sh [impls=python,rust] [idle-seconds=20]
# Env: RENDERER=vulkan|pixman (labwc), GSK=<GSK_RENDERER or empty>, OUTPUTS=WxH,
#      REBUILD_HOME=1, ACTIVITY=1 (menu twice + 8 windows, then ACTIVITY_IDLE s), EXTRA_ENV_RUST/EXTRA_ENV_PYTHON="K=V ...", KEEP_SMAPS=1, SHOT=1,
#      TRAY_ITEMS=N (fake StatusNotifierItems, each with its own IconThemePath)
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
R=/run/user/1000/cbt/rust-mem/n
TEMPLATE=$ROOT/target/realconfig-home
IMPLS=${1:-python,rust}
IDLE=${2:-20}
RUST_DOCK=${RUST_DOCK:-$ROOT/target/release/tessel-dock}
RUST_SHELL=${RUST_SHELL:-$ROOT/target/release/tessel-shell}
if [ ! -d "$TEMPLATE" ] || [ -n "${REBUILD_HOME:-}" ]; then sh "$HERE/make_home.sh" "$TEMPLATE"; fi
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/svc" "$R/tmp"; chmod 700 "$R/rt"
ln -s "$ROOT/target/realconfig-homes" "$R/homes" 2>/dev/null || true
mkdir -p "$ROOT/target/realconfig-homes"
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" \
    -e "s|<standard_session_servicedirs */>|<servicedir>$R/svc</servicedir>|" /usr/share/dbus-1/session.conf > "$R/d.conf"
printf '<labwc_config><core><gap>0</gap><xwaylandPersistence>no</xwaylandPersistence></core></labwc_config>\n' > "$R/cfg/rc.xml"
: > "$R/cfg/autostart"
MODE=${OUTPUTS:-3840x2160}
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
wlr-randr --output HEADLESS-1 --custom-mode $MODE --pos 0,0 >/dev/null 2>&1
wlr-randr --output HEADLESS-2 --custom-mode $MODE --pos ${MODE%x*},0 >/dev/null 2>&1
wlr-randr > "$R/outputs.txt" 2>&1
export DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" GSETTINGS_BACKEND=memory NO_AT_BRIDGE=1 GTK_A11Y=none GDK_BACKEND=wayland
${GSK:+export GSK_RENDERER=$GSK}
RUST_DOCK="$RUST_DOCK" RUST_SHELL="$RUST_SHELL" KEEP_SMAPS="${KEEP_SMAPS:-}" SHOT="${SHOT:-}" ACTIVITY="${ACTIVITY:-}" ACTIVITY_IDLE="${ACTIVITY_IDLE:-10}" ACTIVITY_APPS="${ACTIVITY_APPS:-4}" TRAY_ITEMS="${TRAY_ITEMS:-0}" ACTIVITY_WINDOWS="${ACTIVITY_WINDOWS:-2}" \
EXTRA_ENV_RUST="${EXTRA_ENV_RUST:-}" EXTRA_ENV_PYTHON="${EXTRA_ENV_PYTHON:-}" \
WRAP_RUST_TESSEL_DOCK="${WRAP_RUST_TESSEL_DOCK:-}" WRAP_RUST_TESSEL_SHELL="${WRAP_RUST_TESSEL_SHELL:-}" \
  timeout 900 python3 -I "$HERE/driver.py" "$R" "$TEMPLATE" "$IMPLS" "$IDLE" > "$R/driver.log" 2>&1
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$TEMPLATE" PATH="$PATH" LANG=en_US.UTF-8 XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R/tmp" WLR_BACKENDS=headless \
  WLR_HEADLESS_OUTPUTS=2 WLR_RENDERER="${RENDERER:-vulkan}" WLR_LIBINPUT_NO_DEVICES=1 \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
cat "$R/driver.log"
