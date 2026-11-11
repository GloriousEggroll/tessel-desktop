# Tessel component "lock". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

# Sources come from GitHub: the commit this package is built from, and the
# vendored crates for that commit's Cargo.lock (see README.md, "Sources").
# build.sh overrides both to build the working tree.
%{!?commit:%global commit 2f76e02cc38675edd808f11c574d87a932162c3f}
%{!?vendorlock:%global vendorlock ace8d23feae6}

Name:           tessel-lock
Version:        0.1.0
Release:        3%{?dist}
Summary:        Tessel secure screen lock
License:        GPL-3.0-or-later
URL:            https://github.com/GloriousEggroll/tessel-desktop
Source0:        %{url}/archive/%{commit}/tessel-desktop-%{commit}.tar.gz
Source1:        %{url}/releases/download/vendor-%{vendorlock}/tessel-desktop-vendor-%{vendorlock}.tar.xz

BuildRequires:  cargo
BuildRequires:  gcc
BuildRequires:  pam-libs
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
BuildRequires:  pkgconfig(libudev)
BuildRequires:  pkgconfig(openssl)
BuildRequires:  pkgconfig(pango)
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  pkgconfig(wayland-scanner)
BuildRequires:  pkgconfig(xkbcommon)
BuildRequires:  rust
BuildRequires:  systemd-rpm-macros

Requires:       tessel-common >= %{version}
Requires:       pam
Requires:       tessel-keyboard
Recommends:     fprintd
Recommends:     fprintd-pam

%description
Tessel secure screen lock. Part of the Tessel desktop.

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
cargo build --release --offline --locked -p tessel-lock

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-lock" %{buildroot}%{tessel_libdir}/tessel-lock
ln -s ../lib/tessel/tessel-lock %{buildroot}%{_bindir}/tessel-lock
install -m755 "$CARGO_TARGET_DIR/release/tessel-lock-pam" %{buildroot}%{tessel_libdir}/tessel-lock-pam
ln -s ../lib/tessel/tessel-lock-pam %{buildroot}%{_bindir}/tessel-lock-pam
install -d %{buildroot}%{_userunitdir}
sed 's|%h/.local/lib/tessel|%{tessel_libdir}|g' data/units/tessel-lock.service > %{buildroot}%{_userunitdir}/tessel-lock.service
chmod 644 %{buildroot}%{_userunitdir}/tessel-lock.service
for service in tessel-lock tessel-lock-password tessel-lock-fingerprint; do
    install -Dm644 packaging/defaults/etc/pam.d/$service %{buildroot}%{_sysconfdir}/pam.d/$service
done

%files
%{tessel_libdir}/tessel-lock
%{_bindir}/tessel-lock
%{tessel_libdir}/tessel-lock-pam
%{_bindir}/tessel-lock-pam
%{_userunitdir}/tessel-lock.service
%config(noreplace) %{_sysconfdir}/pam.d/tessel-lock
%config(noreplace) %{_sysconfdir}/pam.d/tessel-lock-password
%config(noreplace) %{_sysconfdir}/pam.d/tessel-lock-fingerprint

%changelog
* Wed Nov 11 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-3
- The "desktop" background includes the default wallpaper
- Window buttons are as far from the side of the window as from its top

* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- Require tessel-common by version, so it can update on its own

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
