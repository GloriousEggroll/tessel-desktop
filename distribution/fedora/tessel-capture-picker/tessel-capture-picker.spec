# Tessel component "capture-picker". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

# Sources come from GitHub: the commit this package is built from, and the
# vendored crates for that commit's Cargo.lock (see README.md, "Sources").
# build.sh overrides both to build the working tree.
%{!?commit:%global commit 9e2c93a5ea13b8c2991f561aae0bae7993f2ee6d}
%{!?vendorlock:%global vendorlock ace8d23feae6}

Name:           tessel-capture-picker
Version:        0.1.0
Release:        2%{?dist}
Summary:        Tessel screen-share picker
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
BuildRequires:  pkgconfig(pango)
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  rust

Requires:       tessel-common >= %{version}
Requires:       xdg-desktop-portal-wlr >= 0.8.4-4
Requires:       xdg-desktop-portal
Requires:       tessel-wayland-helpers

%description
Tessel screen-share picker. Part of the Tessel desktop.

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
* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- Require tessel-common by version, so it can update on its own

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
