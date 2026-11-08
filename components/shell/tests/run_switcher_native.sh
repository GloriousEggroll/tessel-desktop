#!/bin/sh
# Real Alt+Tab tests (tests/switcher_native.rs) and the Alt+Tab latency
# measurement, in a private nested compositor:
#   headless labwc (private XDG_RUNTIME_DIR + private D-Bus)  → its own Xwayland
#   → X11-backend labwc whose keyboard is driven by xdotool XTEST.
# This mirrors test_switcher_native.py: a virtual keyboard's held modifiers do not
# reach layer surfaces through labwc's keyboard group, XTEST does. Nothing here
# touches the live session (wayland-0) or the real session bus.
#   usage: run_switcher_native.sh [test-name …]   (default: all + latency)
set -eu
AGENT=${AGENT:-tessel-shell}
R=/run/user/1000/cbt/rust-$AGENT/x
rm -rf "$R"; mkdir -p "$R/rt" "$R/outer" "$R/inner" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data"; chmod 700 "$R/rt"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
FEATURES=${FEATURES-}
cd "$ROOT"
TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo build -q --release -p tessel-shell $FEATURES --bin tessel-shell-cli
BIN=$(TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo test --release -p tessel-shell $FEATURES --test switcher_native --no-run --message-format=json 2>/dev/null \
      | python3 -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="switcher_native" and m.get("executable"): print(m["executable"])')
REL="$ROOT/target/release"
CLICMD="$REL/tessel-shell-cli"
printf '<labwc_config/>\n' > "$R/outer/rc.xml"
cat > "$R/inner/rc.xml" <<XML
<labwc_config><keyboard><default/>
<keybind key="A-Tab"><action name="Execute" command="$CLICMD switch-window"/></keybind>
<keybind key="A-S-Tab"><action name="Execute" command="$CLICMD switch-window --reverse"/></keybind>
</keyboard></labwc_config>
XML
TESTS=${*:-"hold_tab_repeat_reverse_release_and_fast_tap escape_cancels_and_ignores_more_tabs_until_alt_repressed minimized_window_restores_and_closed_selection_is_safe activities_stays_open_then_alt_tab_uses_normal_release activities_close_targets_windows_without_activation_and_respects_refusal fullscreen_geometry_survives_alt_tab latency"}

cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
OUTER=\$WAYLAND_DISPLAY
[ -n "\$DISPLAY" ] || { echo "no private Xwayland DISPLAY"; labwc --exit; exit 1; }
export GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GSETTINGS_BACKEND=memory GTK_USE_PORTAL=0
export XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data" TESSEL_DOCK_TEST_ISOLATED=1
export TESSEL_SHELL_TEST_CLI="$CLICMD" DBUS_SYSTEM_BUS_ADDRESS=unix:path=/no-tessel-shell-test-system-bus
ls "\$XDG_RUNTIME_DIR" > "$R/before"
WLR_BACKENDS=x11 WLR_X11_OUTPUTS=1 labwc -C "$R/inner" > "$R/inner.log" 2>&1 &
INNER=\$!
for i in \$(seq 300); do
  NEW=\$(ls "\$XDG_RUNTIME_DIR" | grep -E '^wayland-[0-9]+\$' | grep -vxF -f "$R/before" | head -1)
  [ -n "\$NEW" ] && break; sleep 0.02
done
case "\$NEW" in wayland-*) ;; *) echo "inner labwc did not start"; cat "$R/inner.log"; labwc --exit; exit 1;; esac
export WAYLAND_DISPLAY=\$NEW
sleep 0.5
WID=\$(xdotool search --sync --onlyvisible --name "" 2>/dev/null | tail -1)
xdotool windowfocus --sync "\$WID"
status=0
for t in $TESTS; do
  echo "== \$t"
  timeout 60 "$BIN" "\$t" || { echo "FAILED \$t"; status=1; }
done
echo \$status > "$R/result"
kill \$INNER 2>/dev/null; wait \$INNER 2>/dev/null
WAYLAND_DISPLAY=\$OUTER labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/outer" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
grep -E "^(==|test |FAILED|latency_ms|thread|check failed|  )" "$R/labwc.log" || tail -40 "$R/labwc.log"
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
