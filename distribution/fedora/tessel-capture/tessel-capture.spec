# Tessel component "capture". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

Name:           tessel-capture
Version:        0.1.0
Release:        2%{?dist}
Summary:        Tessel screenshots and screen recording
License:        GPL-3.0-or-later
URL:            https://nobaraproject.org
Source0:        tessel-%{version}.tar.gz
Source1:        tessel-%{version}-vendor.tar.xz

BuildRequires:  cargo
BuildRequires:  gcc
BuildRequires:  pkgconfig(cairo)
BuildRequires:  pkgconfig(gdk-pixbuf-2.0)
BuildRequires:  pkgconfig(gio-2.0)
BuildRequires:  pkgconfig(glib-2.0)
BuildRequires:  pkgconfig(gobject-2.0)
BuildRequires:  pkgconfig(graphene-gobject-1.0)
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(gtk4-layer-shell-0)
BuildRequires:  pkgconfig(gtk4-wayland)
BuildRequires:  pkgconfig(libpipewire-0.3)
BuildRequires:  pkgconfig(pango)
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  rust

Requires:       tessel-common >= %{version}
Requires:       wl-clipboard
Requires:       pipewire
Requires:       /usr/bin/ffmpeg
Requires:       /usr/bin/ffprobe
Requires:       xdg-desktop-portal
Requires:       xdg-desktop-portal-wlr >= 0.8.4-4
Requires:       tessel-capture-picker
Requires:       libxkbcommon
Requires:       vulkan-loader
Recommends:     pulseaudio-utils

%description
Tessel screenshots and screen recording. Part of the Tessel desktop.

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
cargo build --release --offline --locked -p tessel-capture

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-capture" %{buildroot}%{tessel_libdir}/tessel-capture
ln -s ../lib/tessel/tessel-capture %{buildroot}%{_bindir}/tessel-capture
# Built by build.rs into the build output: the newest copy is this build's.
helper=$(find "$CARGO_TARGET_DIR/release/build" -name libcapture-pipewire.so -printf '%T@ %p\n' | sort -n | tail -n 1 | cut -d' ' -f2-)
install -m755 "$helper" %{buildroot}%{tessel_libdir}/libcapture-pipewire.so
install -d %{buildroot}%{_datadir}/applications
sed -e 's|@LIBDIR@|%{tessel_libdir}|g' -e 's#^\(Exec\|TryExec\)=tessel-#\1=%{tessel_libdir}/tessel-#' data/applications/org.tessel.Capture.desktop > %{buildroot}%{_datadir}/applications/org.tessel.Capture.desktop
chmod 644 %{buildroot}%{_datadir}/applications/org.tessel.Capture.desktop
install -d %{buildroot}%{_datadir}/applications
sed -e 's|@LIBDIR@|%{tessel_libdir}|g' -e 's#^\(Exec\|TryExec\)=tessel-#\1=%{tessel_libdir}/tessel-#' data/applications/org.tessel.ScreenRecorder.desktop > %{buildroot}%{_datadir}/applications/org.tessel.ScreenRecorder.desktop
chmod 644 %{buildroot}%{_datadir}/applications/org.tessel.ScreenRecorder.desktop

%files
%{tessel_libdir}/tessel-capture
%{_bindir}/tessel-capture
%{tessel_libdir}/libcapture-pipewire.so
%{_datadir}/applications/org.tessel.Capture.desktop
%{_datadir}/applications/org.tessel.ScreenRecorder.desktop

%changelog
* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- Build on 32-bit: warnings in PipeWire headers are not errors

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
