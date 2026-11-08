# Tessel component "wayland-helpers". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

Name:           tessel-wayland-helpers
Version:        0.1.0
Release:        1%{?dist}
Summary:        Tessel Wayland helpers: toplevels, workspaces and overview
License:        GPL-3.0-or-later
URL:            https://nobaraproject.org
Source0:        tessel-%{version}.tar.gz
Source1:        tessel-%{version}-vendor.tar.xz

BuildRequires:  cargo
BuildRequires:  gcc
BuildRequires:  pkgconfig(cairo)
BuildRequires:  pkgconfig(gio-2.0)
BuildRequires:  pkgconfig(glib-2.0)
BuildRequires:  pkgconfig(gobject-2.0)
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  rust

Requires:       tessel-common = %{version}-%{release}
Requires:       labwc >= 0.20.2-17

%description
Tessel Wayland helpers: toplevels, workspaces and overview. Part of the Tessel desktop.

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
cargo build --release --offline --locked -p tessel-wayland

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-dock-toplevel" %{buildroot}%{tessel_libdir}/tessel-dock-toplevel
ln -s ../lib/tessel/tessel-dock-toplevel %{buildroot}%{_bindir}/tessel-dock-toplevel
install -m755 "$CARGO_TARGET_DIR/release/tessel-dock-workspaces" %{buildroot}%{tessel_libdir}/tessel-dock-workspaces
ln -s ../lib/tessel/tessel-dock-workspaces %{buildroot}%{_bindir}/tessel-dock-workspaces
install -m755 "$CARGO_TARGET_DIR/release/tessel-overview" %{buildroot}%{tessel_libdir}/tessel-overview
ln -s ../lib/tessel/tessel-overview %{buildroot}%{_bindir}/tessel-overview
install -m755 "$CARGO_TARGET_DIR/release/tessel-shell-toplevel" %{buildroot}%{tessel_libdir}/tessel-shell-toplevel
ln -s ../lib/tessel/tessel-shell-toplevel %{buildroot}%{_bindir}/tessel-shell-toplevel

%files
%{tessel_libdir}/tessel-dock-toplevel
%{_bindir}/tessel-dock-toplevel
%{tessel_libdir}/tessel-dock-workspaces
%{_bindir}/tessel-dock-workspaces
%{tessel_libdir}/tessel-overview
%{_bindir}/tessel-overview
%{tessel_libdir}/tessel-shell-toplevel
%{_bindir}/tessel-shell-toplevel

%changelog
* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
