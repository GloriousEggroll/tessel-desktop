# Tessel component "welcome". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

# Sources come from GitHub: the commit this package is built from, and the
# vendored crates for that commit's Cargo.lock (see README.md, "Sources").
# build.sh overrides both to build the working tree.
%{!?commit:%global commit e382ef1a484f27dc616642c31651a6972e248240}
%{!?vendorlock:%global vendorlock ace8d23feae6}

Name:           tessel-welcome
Version:        0.1.0
Release:        7%{?dist}
Summary:        Tessel welcome screen
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

Requires:       tessel-common >= %{version}
Requires:       tessel-settings
Requires:       NetworkManager-libnm
Requires:       gstreamer1
Requires:       gstreamer1-plugins-base
Requires:       gstreamer1-plugins-good
Requires:       lcms2
Requires:       libxkbcommon
Requires:       pipewire-gstreamer
Requires:       vulkan-loader
Requires:       wireplumber-libs

%description
Tessel welcome screen. Part of the Tessel desktop.

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
cargo build --release --offline --locked -p welcome

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-welcome" %{buildroot}%{tessel_libdir}/tessel-welcome
ln -s ../lib/tessel/tessel-welcome %{buildroot}%{_bindir}/tessel-welcome
install -d %{buildroot}%{_datadir}/applications
sed -e 's|@LIBDIR@|%{tessel_libdir}|g' -e 's#^\(Exec\|TryExec\)=tessel-#\1=%{tessel_libdir}/tessel-#' data/applications/org.tessel.Welcome.desktop > %{buildroot}%{_datadir}/applications/org.tessel.Welcome.desktop
chmod 644 %{buildroot}%{_datadir}/applications/org.tessel.Welcome.desktop

%files
%{tessel_libdir}/tessel-welcome
%{_bindir}/tessel-welcome
%{_datadir}/applications/org.tessel.Welcome.desktop

%changelog
* Fri Nov 13 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-7
- Rebuilt with the Displays change for HDR without the Vulkan renderer

* Thu Nov 12 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-6
- Keyboard step: turn on Num Lock at sign-in

* Wed Nov 11 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-5
- Layouts use the single system tray

* Wed Nov 11 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-4
- Win and Mac layouts start with one workspace and no workspace indicator
- Media is off in every layout

* Wed Nov 11 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-3
- Window buttons are as far from the side of the window as from its top
- Rounded corners are 8 px, as browsers' own windows have

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- Rebuild with the 32-bit fixes in tessel-capture and tessel-shell

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
