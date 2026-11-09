# Tessel component "battery". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

Name:           tessel-battery
Version:        0.1.0
Release:        2%{?dist}
Summary:        Tessel battery status and charging controls
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
BuildRequires:  systemd-rpm-macros

Requires:       tessel-common >= %{version}
Requires:       upower
Recommends:     tessel-bluetooth

%description
Tessel battery status and charging controls. Part of the Tessel desktop.

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
cargo build --release --offline --locked -p tessel-battery

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-battery" %{buildroot}%{tessel_libdir}/tessel-battery
ln -s ../lib/tessel/tessel-battery %{buildroot}%{_bindir}/tessel-battery
install -m755 "$CARGO_TARGET_DIR/release/tessel-battery-status" %{buildroot}%{tessel_libdir}/tessel-battery-status
ln -s ../lib/tessel/tessel-battery-status %{buildroot}%{_bindir}/tessel-battery-status
install -d %{buildroot}%{_userunitdir}
sed 's|%h/.local/lib/tessel|%{tessel_libdir}|g' data/units/tessel-battery.service > %{buildroot}%{_userunitdir}/tessel-battery.service
chmod 644 %{buildroot}%{_userunitdir}/tessel-battery.service
install -d %{buildroot}%{_datadir}/applications
sed -e 's|@LIBDIR@|%{tessel_libdir}|g' -e 's#^\(Exec\|TryExec\)=tessel-#\1=%{tessel_libdir}/tessel-#' data/applications/org.tessel.BatteryManager.desktop > %{buildroot}%{_datadir}/applications/org.tessel.BatteryManager.desktop
chmod 644 %{buildroot}%{_datadir}/applications/org.tessel.BatteryManager.desktop

%files
%{tessel_libdir}/tessel-battery
%{_bindir}/tessel-battery
%{tessel_libdir}/tessel-battery-status
%{_bindir}/tessel-battery-status
%{_userunitdir}/tessel-battery.service
%{_datadir}/applications/org.tessel.BatteryManager.desktop

%changelog
* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- Require tessel-common by version, so it can update on its own

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
