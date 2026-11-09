# Tessel component "shell". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

Name:           tessel-shell
Version:        0.1.0
Release:        2%{?dist}
Summary:        Tessel Shell: desktop, window switcher, overview and prompts
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
BuildRequires:  systemd-rpm-macros

Requires:       tessel-common >= %{version}
Requires:       labwc >= 0.20.2-17
Requires:       ptyxis
Requires:       wl-clipboard
Requires:       /usr/bin/ffprobe
Requires:       tessel-wayland-helpers
Requires:       wireplumber-libs
Recommends:     mpv
Recommends:     mpvpaper
Recommends:     tessel-files
Recommends:     tessel-settings
Recommends:     tessel-capture

%description
Tessel Shell: desktop, window switcher, overview and prompts. Part of the Tessel desktop.

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
cargo build --release --offline --locked -p tessel-shell

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-shell" %{buildroot}%{tessel_libdir}/tessel-shell
ln -s ../lib/tessel/tessel-shell %{buildroot}%{_bindir}/tessel-shell
install -m755 "$CARGO_TARGET_DIR/release/tessel-shell-cli" %{buildroot}%{tessel_libdir}/tessel-shell-cli
ln -s ../lib/tessel/tessel-shell-cli %{buildroot}%{_bindir}/tessel-shell-cli
install -d %{buildroot}%{_userunitdir}
sed 's|%h/.local/lib/tessel|%{tessel_libdir}|g' data/units/tessel-shell.service > %{buildroot}%{_userunitdir}/tessel-shell.service
chmod 644 %{buildroot}%{_userunitdir}/tessel-shell.service

%files
%{tessel_libdir}/tessel-shell
%{_bindir}/tessel-shell
%{tessel_libdir}/tessel-shell-cli
%{_bindir}/tessel-shell-cli
%{_userunitdir}/tessel-shell.service

%changelog
* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- Build on 32-bit: inode numbers are narrower there

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
