## Safety (hard rules)
- /tmp is FULL (ENOSPC). export TMPDIR=/run/user/1000/cbt/rust-$AGENT (make the dir). Redirect long command output to files there and Read them.
- NEVER restart/kill labwc, the running shell services (waydock, wayshell, notifications, netman, power-manager, polkitman,
  etc.), or any user apps. No `systemctl --user start/stop/restart`, no `pkill`. No sudo/pkexec.
- NEVER run any installer with --apply. Installing is done by the parent at the end. `--check`/preview is ok.
- NEVER send synthetic input, screenshots or windows to the real desktop (WAYLAND_DISPLAY=wayland-0 in /run/user/1000).
  Never derive WAYLAND_DISPLAY from pgrep. GUI tests: use the components' existing `--ui` test mode (private headless
  compositor) with TMPDIR set, or a private nested headless labwc:
    R=/run/user/1000/cbt/rust-$AGENT/n; mkdir -p $R/rt; chmod 700 $R/rt   (keep paths short: unix socket limit)
    dbus config: copy /usr/share/dbus-1/session.conf to $R/d.conf and set <listen>unix:tmpdir=$R</listen>
    env -i HOME=$HOME PATH=$PATH XDG_RUNTIME_DIR=$R/rt TMPDIR=$R WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
      WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file=$R/d.conf -- labwc -C <cfgdir> -s $R/run.sh
    run.sh must begin with:  case "$XDG_RUNTIME_DIR:$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
- Don't touch PAM/system files under /etc. Don't modify ~/.config live files (parent installs). Read-only inspection is fine.

## Code
