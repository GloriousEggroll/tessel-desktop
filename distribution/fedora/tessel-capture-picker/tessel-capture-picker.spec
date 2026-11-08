# Tessel component "capture-picker". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

Name:           tessel-capture-picker
Version:        0.1.0
Release:        1%{?dist}
Summary:        Tessel screen-share picker
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
BuildRequires:  pkgconfig(pango)
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  rust

Requires:       tessel-common = %{version}-%{release}
Requires:       xdg-desktop-portal-wlr >= 0.8.4-4
Requires:       xdg-desktop-portal
Requires:       tessel-wayland-helpers

%description
Tessel screen-share picker. Part of the Tessel desktop.

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
cargo build --release --offline --locked -p tessel-capture-picker

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-capture-picker" %{buildroot}%{tessel_libdir}/tessel-capture-picker
ln -s ../lib/tessel/tessel-capture-picker %{buildroot}%{_bindir}/tessel-capture-picker
install -Dm644 packaging/defaults/etc/xdg/xdg-desktop-portal-wlr/labwc %{buildroot}%{_sysconfdir}/xdg/xdg-desktop-portal-wlr/labwc
install -Dm644 packaging/defaults/etc/xdg/xdg-desktop-portal-wlr/wlroots %{buildroot}%{_sysconfdir}/xdg/xdg-desktop-portal-wlr/wlroots

%files
%{tessel_libdir}/tessel-capture-picker
%{_bindir}/tessel-capture-picker
%dir %{_sysconfdir}/xdg/xdg-desktop-portal-wlr
%config(noreplace) %{_sysconfdir}/xdg/xdg-desktop-portal-wlr/labwc
%config(noreplace) %{_sysconfdir}/xdg/xdg-desktop-portal-wlr/wlroots

%changelog
* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
