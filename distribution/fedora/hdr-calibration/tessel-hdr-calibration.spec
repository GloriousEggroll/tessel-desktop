# Tessel component "hdr-calibration". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

Name:           tessel-hdr-calibration
Version:        0.1.0
Release:        1%{?dist}
Summary:        Tessel HDR calibration
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
BuildRequires:  pkgconfig(pango)
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  rust

Requires:       tessel-common = %{version}-%{release}
Requires:       labwc >= 0.20.2-17

%description
Tessel HDR calibration. Part of the Tessel desktop.

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
cargo build --release --offline --locked -p tessel-hdr-calibration

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-hdr-calibration" %{buildroot}%{tessel_libdir}/tessel-hdr-calibration
ln -s ../lib/tessel/tessel-hdr-calibration %{buildroot}%{_bindir}/tessel-hdr-calibration
install -d %{buildroot}%{_datadir}/applications
sed -e 's|@LIBDIR@|%{tessel_libdir}|g' -e 's#^\(Exec\|TryExec\)=tessel-#\1=%{tessel_libdir}/tessel-#' data/applications/org.tessel.HDRCalibration.desktop > %{buildroot}%{_datadir}/applications/org.tessel.HDRCalibration.desktop
chmod 644 %{buildroot}%{_datadir}/applications/org.tessel.HDRCalibration.desktop

%files
%{tessel_libdir}/tessel-hdr-calibration
%{_bindir}/tessel-hdr-calibration
%{_datadir}/applications/org.tessel.HDRCalibration.desktop

%changelog
* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
