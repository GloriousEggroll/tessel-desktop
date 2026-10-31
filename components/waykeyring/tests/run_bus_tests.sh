#!/bin/sh
# Run tests/bus_test.py against waykeyring on a private session bus (no
# standard service dirs, own XDG dirs, kernel keyring and unix_chkpwd off).
# Never touches the real session bus, keyring or ~/.local/share.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
(cd "$ROOT" && cargo build -q -p waykeyring)
R=/run/user/1000/cbt/rust-keyring
rm -rf "$R"; mkdir -p "$R/rt" "$R/data" "$R/config"; chmod 700 "$R/rt"
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" \
    /usr/share/dbus-1/session.conf > "$R/d.conf"
exec env -i HOME="$R" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" XDG_DATA_HOME="$R/data" XDG_CONFIG_HOME="$R/config" \
  USER="${USER:-tester}" TMPDIR="$R" GIO_USE_VFS=local GDK_DEBUG=no-portals GTK_USE_PORTAL=0 \
  dbus-run-session --config-file="$R/d.conf" -- python3 -I -u "$HERE/bus_test.py" "$ROOT/target/debug/waykeyring" "$R"
