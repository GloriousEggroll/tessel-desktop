# Tessel component "dock". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

Name:           tessel-dock
Version:        0.1.0
Release:        2%{?dist}
Summary:        Tessel Dock: application dock, panel and system tray
License:        GPL-3.0-or-later
URL:            https://nobaraproject.org
Source0:        tessel-%{version}.tar.gz
Source1:        tessel-%{version}-vendor.tar.xz

BuildRequires:  cargo
BuildRequires:  gcc
BuildRequires:  pkgconfig(cairo)
BuildRequires:  pkgconfig(gdk-pixbuf-2.0)
BuildRequires:  pkgconfig(gio-2.0)
BuildRequires:  pkgconfig(gio-unix-2.0)
BuildRequires:  pkgconfig(glib-2.0)
BuildRequires:  pkgconfig(gobject-2.0)
BuildRequires:  pkgconfig(graphene-gobject-1.0)
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(gtk4-layer-shell-0)
BuildRequires:  pkgconfig(gtk4-wayland)
BuildRequires:  pkgconfig(openssl)
BuildRequires:  pkgconfig(pango)
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  rust
BuildRequires:  systemd-rpm-macros

Requires:       tessel-common >= %{version}
Requires:       labwc >= 0.20.2-17
Requires:       tessel-wayland-helpers
Requires:       tessel-shell
Requires:       pipewire-utils
Requires:       wireplumber
Requires:       xorg-x11-server-Xwayland
Recommends:     lm_sensors
Recommends:     /usr/bin/ffmpeg
Recommends:     tessel-capture
Recommends:     tessel-media
Recommends:     tessel-session

%description
Tessel Dock: application dock, panel and system tray. Part of the Tessel desktop.

%prep
%autosetup -n tessel-%{version} -a1
cat >> .cargo/config.toml <<'EOF'

[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF

%build
export TESSEL_PACKAGED=1
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
export CARGO_HOME=%{_builddir}/cargo-home
export RUSTFLAGS="-Clink-arg=-Wl,-z,relro -Clink-arg=-Wl,-z,now"
cargo build --release --offline --locked -p tessel-dock

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-capslock-status" %{buildroot}%{tessel_libdir}/tessel-capslock-status
ln -s ../lib/tessel/tessel-capslock-status %{buildroot}%{_bindir}/tessel-capslock-status
install -m755 "$CARGO_TARGET_DIR/release/tessel-dock" %{buildroot}%{tessel_libdir}/tessel-dock
ln -s ../lib/tessel/tessel-dock %{buildroot}%{_bindir}/tessel-dock
install -m755 "$CARGO_TARGET_DIR/release/tessel-dock-launch" %{buildroot}%{tessel_libdir}/tessel-dock-launch
ln -s ../lib/tessel/tessel-dock-launch %{buildroot}%{_bindir}/tessel-dock-launch
install -d %{buildroot}%{_userunitdir}
sed 's|%h/.local/lib/tessel|%{tessel_libdir}|g' data/units/tessel-dock.service > %{buildroot}%{_userunitdir}/tessel-dock.service
chmod 644 %{buildroot}%{_userunitdir}/tessel-dock.service
install -d %{buildroot}%{_datadir}/applications
sed -e 's|@LIBDIR@|%{tessel_libdir}|g' -e 's#^\(Exec\|TryExec\)=tessel-#\1=%{tessel_libdir}/tessel-#' data/applications/org.tessel.DockSettings.desktop > %{buildroot}%{_datadir}/applications/org.tessel.DockSettings.desktop
chmod 644 %{buildroot}%{_datadir}/applications/org.tessel.DockSettings.desktop

%files
%{tessel_libdir}/tessel-capslock-status
%{_bindir}/tessel-capslock-status
%{tessel_libdir}/tessel-dock
%{_bindir}/tessel-dock
%{tessel_libdir}/tessel-dock-launch
%{_bindir}/tessel-dock-launch
%{_userunitdir}/tessel-dock.service
%{_datadir}/applications/org.tessel.DockSettings.desktop

%changelog
* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- Require tessel-common by version, so it can update on its own
- New users start on the Win layout

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
