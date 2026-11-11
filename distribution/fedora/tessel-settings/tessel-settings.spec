# Tessel component "settings". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

# Sources come from GitHub: the commit this package is built from, and the
# vendored crates for that commit's Cargo.lock (see README.md, "Sources").
# build.sh overrides both to build the working tree.
%{!?commit:%global commit d0ec5d273dc11147f419a055471e5915f6f80bf5}
%{!?vendorlock:%global vendorlock ace8d23feae6}

Name:           tessel-settings
Version:        0.1.0
Release:        10%{?dist}
Summary:        Tessel Global Settings and night light
License:        GPL-3.0-or-later
URL:            https://github.com/GloriousEggroll/tessel-desktop
Source0:        %{url}/archive/%{commit}/tessel-desktop-%{commit}.tar.gz
Source1:        %{url}/releases/download/vendor-%{vendorlock}/tessel-desktop-vendor-%{vendorlock}.tar.xz

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
BuildRequires:  pkgconfig(libpipewire-0.3)
BuildRequires:  pkgconfig(libudev)
BuildRequires:  pkgconfig(openssl)
BuildRequires:  pkgconfig(pango)
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  pkgconfig(wayland-scanner)
BuildRequires:  pkgconfig(xkbcommon)
BuildRequires:  rust
BuildRequires:  systemd-rpm-macros

Requires:       tessel-common >= %{version}
Requires:       glib2
Requires:       libinput
Requires:       pciutils
Requires:       pipewire-utils
Requires:       wlr-randr
Requires:       wlsunset
Requires:       systemd
Requires:       NetworkManager-libnm
Requires:       gstreamer1
Requires:       gstreamer1-plugins-base
Requires:       gstreamer1-plugins-good
Requires:       lcms2
Requires:       libxkbcommon
Requires:       pipewire-gstreamer
Requires:       vulkan-loader
Requires:       wireplumber-libs
Recommends:     flatpak
Recommends:     brightnessctl
Recommends:     playerctl
Recommends:     pulseaudio-utils

%description
Tessel Global Settings and night light. Part of the Tessel desktop.

%prep
%autosetup -n tessel-desktop-%{commit} -a1
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
cargo build --release --offline --locked -p tessel-settings

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-night-light" %{buildroot}%{tessel_libdir}/tessel-night-light
ln -s ../lib/tessel/tessel-night-light %{buildroot}%{_bindir}/tessel-night-light
install -m755 "$CARGO_TARGET_DIR/release/tessel-settings" %{buildroot}%{tessel_libdir}/tessel-settings
ln -s ../lib/tessel/tessel-settings %{buildroot}%{_bindir}/tessel-settings
install -d %{buildroot}%{_userunitdir}
sed 's|%h/.local/lib/tessel|%{tessel_libdir}|g' data/units/tessel-night-light.service > %{buildroot}%{_userunitdir}/tessel-night-light.service
chmod 644 %{buildroot}%{_userunitdir}/tessel-night-light.service
install -d %{buildroot}%{_datadir}/applications
sed -e 's|@LIBDIR@|%{tessel_libdir}|g' -e 's#^\(Exec\|TryExec\)=tessel-#\1=%{tessel_libdir}/tessel-#' data/applications/org.tessel.GlobalSettings.desktop > %{buildroot}%{_datadir}/applications/org.tessel.GlobalSettings.desktop
chmod 644 %{buildroot}%{_datadir}/applications/org.tessel.GlobalSettings.desktop

%files
%{tessel_libdir}/tessel-night-light
%{_bindir}/tessel-night-light
%{tessel_libdir}/tessel-settings
%{_bindir}/tessel-settings
%{_userunitdir}/tessel-night-light.service
%{_datadir}/applications/org.tessel.GlobalSettings.desktop

%changelog
* Tue Nov 10 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-10
- Layouts: add Tessel, the desktop's own layout (one top bar in islands)

* Tue Nov 10 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-9
- Layouts: the Ubuntu-style GNOME layout is now GNOME (Ubuntu); add a stock GNOME layout

* Tue Nov 10 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-8
- Date & Time page: the current time shows every second and is easier to read

* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-7
- Displays page: keep a turned display's turned size in the layout preview

* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-6
- The GNOME, KDE and Mac layouts have no monitor-specific settings

* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-5
- The Win layout has no monitor-specific settings

* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-4
- New users start on the Win layout

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-3
- Displays: Apply asks for 10 seconds whether to keep display settings, and reverts them otherwise

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- Rebuild with the 32-bit fixes in tessel-capture and tessel-shell

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
