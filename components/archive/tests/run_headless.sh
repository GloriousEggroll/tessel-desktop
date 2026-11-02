#!/bin/sh
# Run tests/ui_headless.rs inside a private nested headless labwc
# (tools/headless.md). Never touches the live session: own XDG_RUNTIME_DIR and
# Wayland socket, own D-Bus session bus without service directories (nothing
# can be D-Bus-activated: no gvfs, no portal; the run fails if anything is),
# GIO_USE_VFS=local, private HOME and XDG config/cache/data/state, no input
# devices (the tests drive the private compositor's virtual pointer/keyboard
# only). Every archive fixture lives under $R. Extra arguments filter checks.
# WAYARCHIVE_UI_SHOTS=<dir> saves screenshots there.
set -eu
AGENT=${AGENT:-archive}
BASE=/run/user/1000/cbt/rust-$AGENT
R=$BASE/n
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/tmp" "$R/home" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data" "$R/xdg/state" "$R/bin"; chmod 700 "$R/rt"
# Fixed places for the drag-and-drop check: the archive at the left, Files at the right.
cat > "$R/cfg/rc.xml" <<'XML'
<?xml version="1.0"?>
<labwc_config>
  <windowRules>
    <windowRule identifier="org.tessel.ArchiveManager" title="*dnd-source*">
      <action name="MoveTo" x="0" y="0" />
      <action name="ResizeTo" width="620" height="700" />
    </windowRule>
    <windowRule identifier="org.tessel.Files">
      <action name="MoveTo" x="650" y="0" />
      <action name="ResizeTo" width="620" height="700" />
    </windowRule>
  </windowRules>
</labwc_config>
XML
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
# Launch fakes: nothing may start a real application from a test.
for tool in systemd-run xdg-terminal-exec xdg-open gio; do
  printf '#!/bin/sh\necho "%s $*" >> "%s/tmp/calls.log"\nexit 0\n' "$tool" "$R" > "$R/bin/$tool"
done
chmod +x "$R/bin/"*
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
export TMPDIR=$BASE
(cd "$ROOT" && cargo build -q -p tessel-archive --bin tessel-archive && cargo build -q -p tessel-files --bin tessel-files) || { echo "build failed"; exit 1; }
BIN=$(cd "$ROOT" && cargo test -p tessel-archive --test ui_headless --no-run --message-format=json 2>/dev/null \
      | jq -r 'select(.reason == "compiler-artifact" and .target.name == "ui_headless" and .executable) | .executable')
[ -n "$BIN" ] || { echo "build failed"; exit 1; }
WAYFILES="$ROOT/target/debug/tessel-files"
WAYARCHIVE="$ROOT/target/debug/tessel-archive"
rm -f "$R/result"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
PATH="$R/bin:\$PATH" HOME="$R/home" WAYARCHIVE_UI_ISOLATED_TEST=1 WAYARCHIVE_UI_SHOTS="${WAYARCHIVE_UI_SHOTS:-}" \
WAYFILES_BIN="$WAYFILES" WAYARCHIVE_BIN="$WAYARCHIVE" GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo \
GSETTINGS_BACKEND=memory GTK_USE_PORTAL=0 GDK_DEBUG=no-portals GIO_USE_VFS=local TMPDIR="$R/tmp" XDG_CONFIG_HOME="$R/xdg/config" \
XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data" XDG_STATE_HOME="$R/xdg/state" XDG_CONFIG_DIRS="$R/none" \
XDG_DATA_DIRS="$R/xdg/data:/usr/share" DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" \
timeout 900 "$BIN" $* > "$R/log" 2>&1; echo \$? > "$R/result"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -u WAYLAND_DISPLAY -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS -u TMPDIR -i HOME="$R/home" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" \
  TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
cat "$R/log"
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
