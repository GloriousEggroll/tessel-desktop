# Tessel component "power". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

Name:           tessel-power
Version:        0.1.0
Release:        1%{?dist}
Summary:        Tessel power settings and session idle policy
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
Requires:       swayidle
Requires:       wlopm
Requires:       wlr-randr
Requires:       upower
Recommends:     power-profiles-daemon
Recommends:     tessel-lock
Recommends:     tessel-session
Recommends:     tessel-bluetooth

%description
Tessel power settings and session idle policy. Part of the Tessel desktop.

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
cargo build --release --offline --locked -p tessel-power

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-power" %{buildroot}%{tessel_libdir}/tessel-power
ln -s ../lib/tessel/tessel-power %{buildroot}%{_bindir}/tessel-power
install -m755 "$CARGO_TARGET_DIR/release/tessel-power-status" %{buildroot}%{tessel_libdir}/tessel-power-status
ln -s ../lib/tessel/tessel-power-status %{buildroot}%{_bindir}/tessel-power-status
install -d %{buildroot}%{_userunitdir}
sed 's|%h/.local/lib/tessel|%{tessel_libdir}|g' data/units/tessel-power.service > %{buildroot}%{_userunitdir}/tessel-power.service
chmod 644 %{buildroot}%{_userunitdir}/tessel-power.service
install -d %{buildroot}%{_datadir}/applications
sed -e 's|@LIBDIR@|%{tessel_libdir}|g' -e 's#^\(Exec\|TryExec\)=tessel-#\1=%{tessel_libdir}/tessel-#' data/applications/org.tessel.PowerManager.desktop > %{buildroot}%{_datadir}/applications/org.tessel.PowerManager.desktop
chmod 644 %{buildroot}%{_datadir}/applications/org.tessel.PowerManager.desktop

%files
%{tessel_libdir}/tessel-power
%{_bindir}/tessel-power
%{tessel_libdir}/tessel-power-status
%{_bindir}/tessel-power-status
%{_userunitdir}/tessel-power.service
%{_datadir}/applications/org.tessel.PowerManager.desktop

%changelog
* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
