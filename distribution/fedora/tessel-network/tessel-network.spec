# Tessel component "network". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

Name:           tessel-network
Version:        0.1.0
Release:        1%{?dist}
Summary:        Tessel network manager
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

Requires:       tessel-common = %{version}-%{release}
Requires:       NetworkManager
Requires:       nm-connection-editor
Requires:       NetworkManager-libnm
Recommends:     wireguard-tools
Recommends:     NetworkManager-openvpn

%description
Tessel network manager. Part of the Tessel desktop.

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
cargo build --release --offline --locked -p tessel-network

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-network" %{buildroot}%{tessel_libdir}/tessel-network
ln -s ../lib/tessel/tessel-network %{buildroot}%{_bindir}/tessel-network
install -m755 "$CARGO_TARGET_DIR/release/tessel-network-status" %{buildroot}%{tessel_libdir}/tessel-network-status
ln -s ../lib/tessel/tessel-network-status %{buildroot}%{_bindir}/tessel-network-status
install -d %{buildroot}%{_userunitdir}
sed 's|%h/.local/lib/tessel|%{tessel_libdir}|g' data/units/tessel-network.service > %{buildroot}%{_userunitdir}/tessel-network.service
chmod 644 %{buildroot}%{_userunitdir}/tessel-network.service
install -d %{buildroot}%{_datadir}/applications
sed -e 's|@LIBDIR@|%{tessel_libdir}|g' -e 's#^\(Exec\|TryExec\)=tessel-#\1=%{tessel_libdir}/tessel-#' data/applications/org.tessel.NetworkManager.desktop > %{buildroot}%{_datadir}/applications/org.tessel.NetworkManager.desktop
chmod 644 %{buildroot}%{_datadir}/applications/org.tessel.NetworkManager.desktop

%files
%{tessel_libdir}/tessel-network
%{_bindir}/tessel-network
%{tessel_libdir}/tessel-network-status
%{_bindir}/tessel-network-status
%{_userunitdir}/tessel-network.service
%{_datadir}/applications/org.tessel.NetworkManager.desktop

%changelog
* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
