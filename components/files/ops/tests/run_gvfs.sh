#!/bin/sh
# Real GVfs archive/Trash checks for tessel-files-ops (tests/gvfs.rs, port of test_file_gvfs.py).
#
# Isolation, in order:
#  * private HOME / XDG_{CONFIG,DATA,CACHE}_HOME / XDG_RUNTIME_DIR below a fresh directory;
#  * a private dbus-daemon whose only <servicedir> holds two generated service files
#    (org.gtk.vfs.Daemon -> /usr/libexec/gvfsd, org.gtk.vfs.Metadata -> gvfsd-metadata
#    writing only below the private XDG_DATA_HOME); no <standard_session_servicedirs/>;
#    the run fails if any other service is activated;
#  * a private user+mount namespace in which every mounted filesystem that carries a
#    real .Trash or .Trash-<uid> directory is covered by an empty read-only tmpfs, so
#    gvfsd-trash can neither list nor change the user's real Trash on other volumes;
#  * no system bus (DBUS_SYSTEM_BUS_ADDRESS points at a missing socket), no FUSE,
#    the unix volume monitor only.
# Usage: tests/run_gvfs.sh [scratch-dir]
set -eu

here=$(cd "$(dirname "$0")" && pwd)
workspace=$(cd "$here/../../../.." && pwd)
scratch=${1:-/run/user/$(id -u)/cbt/rust-file-manager-ops}
mkdir -p "$scratch"
uid=$(id -u)
gid=$(id -g)

for tool in dbus-run-session unshare /usr/libexec/gvfsd /usr/libexec/gvfsd-trash /usr/libexec/gvfsd-archive; do
    command -v "$tool" >/dev/null 2>&1 || { echo "run_gvfs.sh: $tool is missing; GVfs checks cannot run" >&2; exit 2; }
done

# Build first, outside the namespaces, with the caller's environment.
build_log="$scratch/gvfs-build.log"
(cd "$workspace" && TMPDIR="$scratch" cargo test -p tessel-files-ops --test gvfs --no-run) >"$build_log" 2>&1 \
    || { cat "$build_log" >&2; exit 1; }
binary=$(sed -n 's/.*Executable tests\/gvfs\.rs (\(.*\))$/\1/p' "$build_log" | tail -n 1)
case $binary in /*) ;; *) binary="$workspace/$binary" ;; esac
[ -x "$binary" ] || { echo "run_gvfs.sh: test binary not found" >&2; cat "$build_log" >&2; exit 1; }

root=$(mktemp -d "$scratch/g.XXXXXX")
trap 'rm -rf "$root"' EXIT
for folder in home config data cache runtime services; do
    mkdir -m 700 "$root/$folder"
done

cat >"$root/services/org.gtk.vfs.Daemon.service" <<EOF
[D-BUS Service]
Name=org.gtk.vfs.Daemon
Exec=/usr/libexec/gvfsd
EOF

cat >"$root/services/org.gtk.vfs.Metadata.service" <<EOF
[D-BUS Service]
Name=org.gtk.vfs.Metadata
Exec=/usr/libexec/gvfsd-metadata
EOF

cat >"$root/bus.conf" <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <keep_umask/>
  <listen>unix:dir=$root</listen>
  <auth>EXTERNAL</auth>
  <servicedir>$root/services</servicedir>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF

cat >"$root/inner.sh" <<'EOF'
#!/bin/sh
# Runs as namespace root: hide every filesystem that has a real Trash directory.
set -eu
uid=$1; gid=$2; root=$3; shift 3
hidden=0
for raw in $(awk '{print $5}' /proc/self/mountinfo | sort -u); do
    mountpoint=$(printf '%b' "$raw")
    case $mountpoint in /proc|/proc/*|/sys|/sys/*|/dev|/dev/*) continue ;; esac
    case $root/ in "$mountpoint"/*) inside=1 ;; *) inside=0 ;; esac
    for trash in "$mountpoint/.Trash-$uid" "$mountpoint/.Trash"; do
        if [ -e "$trash" ]; then
            if [ "$inside" = 1 ] || [ "$mountpoint" = / ]; then
                echo "run_gvfs.sh: the fixture filesystem $mountpoint has a real $trash; refusing" >&2
                exit 3
            fi
            mount -t tmpfs -o size=64k,mode=0555 tessel-files-hidden "$mountpoint"
            hidden=$((hidden + 1))
            break
        fi
    done
done
for raw in $(awk '{print $5}' /proc/self/mountinfo | sort -u); do
    mountpoint=$(printf '%b' "$raw")
    for trash in "$mountpoint/.Trash-$uid" "$mountpoint/.Trash"; do
        [ -e "$trash" ] && { echo "run_gvfs.sh: $trash is still visible; refusing" >&2; exit 3; }
    done
done
echo "run_gvfs.sh: hid $hidden filesystem(s) with a real Trash directory"
exec unshare --user --map-user="$uid" --map-group="$gid" "$@"
EOF
chmod 700 "$root/inner.sh"

bus_log="$root/bus.log"
status=0
env -i PATH="$PATH" HOME="$root/home" XDG_CONFIG_HOME="$root/config" XDG_DATA_HOME="$root/data" \
    XDG_CACHE_HOME="$root/cache" XDG_RUNTIME_DIR="$root/runtime" LANG=C.UTF-8 \
    WAYFILES_GVFS_ISOLATED=1 WAYFILES_GVFS_TEST_ROOT="$root" \
    DBUS_SYSTEM_BUS_ADDRESS="unix:path=$root/no-system-bus" \
    GIO_USE_VFS=gvfs GIO_USE_VOLUME_MONITOR=unix GVFS_DISABLE_FUSE=1 \
    GDK_DEBUG=no-portals GTK_USE_PORTAL=0 \
    unshare --user --map-root-user --mount "$root/inner.sh" "$uid" "$gid" "$root" \
    dbus-run-session --config-file="$root/bus.conf" -- "$binary" --test-threads=1 2>"$bus_log" || status=$?
cat "$bus_log" >&2
if grep 'Activating service' "$bus_log" | grep -v -e "name=.org.gtk.vfs.Daemon." -e "name=.org.gtk.vfs.Metadata." >/dev/null; then
    echo "run_gvfs.sh: an unexpected D-Bus service was activated" >&2
    status=1
fi
if grep -q 'skipping' "$bus_log"; then
    echo "run_gvfs.sh: some GVfs checks skipped themselves" >&2
    status=1
fi
exit $status
