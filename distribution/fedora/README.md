# Fedora / Nobara packages

One folder per packaged part, each with its `.spec`:

- a folder per component in `components/` that ships programs (`dock/` makes `tessel-dock`);
- `wayland-helpers/`: the programs of `crates/wayland`;
- `common/`: translations, the logo, the program directory and `packaging/defaults`;
- `tessel-wallpapers/`: the Slate wallpaper of `branding/wallpaper`;
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

## Sources

Specs fetch their sources from https://github.com/GloriousEggroll/tessel-desktop:

- `Source0`: `archive/<commit>/tessel-desktop-<commit>.tar.gz`, the tree at the spec's `%commit`;
- `Source1`: `releases/download/vendor-<lock>/tessel-desktop-vendor-<lock>.tar.xz`, the Rust crates
  for that commit's `Cargo.lock` (`<lock>` is the first 12 hex digits of its SHA-256), so builds need
  no network. GitHub does not make this file: upload the one `build.sh` leaves in
  `build-fedora/sources/` to a release with the tag `vendor-<lock>`. It only changes with `Cargo.lock`.

`branding/wallpaper` is left out of that archive (`.gitattributes`), because its pictures are large
and only one package needs them. `tessel-wallpapers` names each picture as a source of its own,
`raw/<commit>/branding/wallpaper/renders/<file>`: the file at the spec's `%commit`.

`build.sh` makes all of these from the local repository under those names, so the commit must be
in the local repository; push it before submitting the SRPMs. `TREE=1 build.sh` builds the working
tree instead, for testing only.

## Updating one package

Packages require each other by version, not release, so a fix needs only the packages it changes.
Commit the fix, point those specs at it with `set-commit.sh <commit> <folder> ...`, raise their
`Release:`, build them with `build.sh <folder> ...`, push, and submit those SRPMs. If a
package needs another's fix, require that release explicitly, as `tessel-meta` does.
Raise `Version:` in every spec together, because every package requires at least that version.

## What differs from `tools/install.py`

- Programs are in `/usr/lib/tessel`, linked from `/usr/bin`. Specs set `TESSEL_PACKAGED=1` at build
  time, which points generated configuration at those paths (`tessel_core::paths::PACKAGED`).
- The session entry runs `tessel-session-launch`, which sets up each user's labwc files once per
  package release, then starts labwc. A user with a `~/.local` install is left alone.
- `tessel-login` runs its own greetd instance with `/etc/greetd/tessel-config.toml`. greetd's own
  service and `/etc/greetd/config.toml` are not changed. A first install enables it when no display
  manager is enabled, or in place of `greetd.service`; another display manager is left alone.
  When there was no display manager at all and the system was set to start to the text console
  (`multi-user.target`), that first install also sets `graphical.target`, since the login screen
  would otherwise be enabled but never started; any other default target is left alone.
  Updates never change this choice, and it takes effect at the next boot.
  - To use the Tessel login screen: `systemctl enable --force tessel-login.service`
  - To use normal greetd: `systemctl disable tessel-login.service`, then `systemctl enable --force greetd.service`
- `tessel-keyring` conflicts with `gnome-keyring`: both own the `org.freedesktop.secrets` activation file.
