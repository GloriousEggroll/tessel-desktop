# Fedora / Nobara packages

One folder per packaged part, each with its `.spec`:

- a folder per component in `components/` that ships programs (`dock/` makes `tessel-dock`);
- `wayland-helpers/`: the programs of `crates/wayland`;
- `common/`: translations, the logo, the program directory and `packaging/defaults`;
- `tessel-meta/`: requires all of the above, plus labwc, Ptyxis and xdg-desktop-portal-wlr.

`components/theme` is a library with no programs. `components/home-assistant-screen` (Kasumi)
is its own device with its own installer. labwc, Ptyxis and xdg-desktop-portal-wlr come from the Nobara repositories.

## Building

    distribution/fedora/build.sh              # everything, each in a clean chroot as on COPR
    FAST=1 distribution/fedora/build.sh       # one shared chroot: quick, but hides missing BuildRequires
    distribution/fedora/build.sh dock shell   # only these

The results land in `build-fedora/` (not tracked by git): `RPMS/`, `SRPMS/`, `logs/<package>/` and `sources/`.
A full build ends with `check.sh`, which fails if a program, unit or desktop file from the
repository is in no package or in more than one. **A new program, unit or desktop file must be added
to its component's spec.**

Every spec takes the same two sources, which `build.sh` makes: `tessel-<version>.tar.gz` (the
source tree) and `tessel-<version>-vendor.tar.xz` (the Rust crates, so builds need no network).

## Updating one package

Packages require each other by version, not release, so a fix needs only the packages it changes.
Raise `Release:` in those specs, build them with `build.sh <folder> ...` and submit those SRPMs. If a
package needs another's fix, require that release explicitly, as `tessel-meta` does.
Raise `Version:` in every spec together, because every package requires at least that version.

## What differs from `tools/install.py`

- Programs are in `/usr/lib/tessel`, linked from `/usr/bin`. Specs set `TESSEL_PACKAGED=1` at build
  time, which points generated configuration at those paths (`tessel_core::paths::PACKAGED`).
- The session entry runs `tessel-session-launch`, which sets up each user's labwc files once per
  package release, then starts labwc. A user with a `~/.local` install is left alone.
- `tessel-login` is installed but not enabled:
  `systemctl enable --force tessel-login.service` switches the login screen at the next boot.
- `tessel-keyring` conflicts with `gnome-keyring`: both own the `org.freedesktop.secrets` activation file.
