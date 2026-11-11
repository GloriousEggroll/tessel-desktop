# Tessel component "keyring". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

# Sources come from GitHub: the commit this package is built from, and the
# vendored crates for that commit's Cargo.lock (see README.md, "Sources").
# build.sh overrides both to build the working tree.
%{!?commit:%global commit 219a63a92b10f8502a8b97012868f4524014fc05}
%{!?vendorlock:%global vendorlock ace8d23feae6}

Name:           tessel-keyring
Version:        0.1.0
Release:        3%{?dist}
Summary:        Tessel saved passwords (Secret Service)
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
Requires:       pam
Requires:       xdg-desktop-portal
Conflicts:      gnome-keyring

%description
Tessel saved passwords (Secret Service). Part of the Tessel desktop.

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
cargo build --release --offline --locked -p tessel-keyring -p tessel-keyring-ui -p pam-tessel-keyring

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-keyring" %{buildroot}%{tessel_libdir}/tessel-keyring
ln -s ../lib/tessel/tessel-keyring %{buildroot}%{_bindir}/tessel-keyring
install -m755 "$CARGO_TARGET_DIR/release/tessel-keyring-prompt" %{buildroot}%{tessel_libdir}/tessel-keyring-prompt
ln -s ../lib/tessel/tessel-keyring-prompt %{buildroot}%{_bindir}/tessel-keyring-prompt
install -d %{buildroot}%{_datadir}/applications
sed -e 's|@LIBDIR@|%{tessel_libdir}|g' -e 's#^\(Exec\|TryExec\)=tessel-#\1=%{tessel_libdir}/tessel-#' components/keyring/data/org.tessel.Passwords.desktop > %{buildroot}%{_datadir}/applications/org.tessel.Passwords.desktop
chmod 644 %{buildroot}%{_datadir}/applications/org.tessel.Passwords.desktop
sed 's|^ExecStart=.*|ExecStart=%{tessel_libdir}/tessel-keyring|' components/keyring/data/tessel-keyring.service > tessel-keyring.service
install -Dm644 tessel-keyring.service %{buildroot}%{_userunitdir}/tessel-keyring.service
sed 's|^Exec=.*|Exec=%{tessel_libdir}/tessel-keyring|' components/keyring/data/org.freedesktop.secrets.service > org.freedesktop.secrets.service
install -Dm644 org.freedesktop.secrets.service %{buildroot}%{_datadir}/dbus-1/services/org.freedesktop.secrets.service
install -Dm644 packaging/defaults/usr/share/xdg-desktop-portal/portals/tessel-keyring.portal %{buildroot}%{_datadir}/xdg-desktop-portal/portals/tessel-keyring.portal
install -Dm755 "$CARGO_TARGET_DIR/release/libpam_tessel_keyring.so" %{buildroot}%{_libdir}/security/pam_tessel_keyring.so

%post
# Unlock saved passwords when the login password changes. The leading "-" makes
# PAM skip the line if the module is ever missing.
if [ -f %{_sysconfdir}/pam.d/passwd ] && ! grep -q pam_tessel_keyring.so %{_sysconfdir}/pam.d/passwd; then
    sed -i '/^password[[:space:]]\+substack[[:space:]]\+system-auth/a -password   optional\tpam_tessel_keyring.so' %{_sysconfdir}/pam.d/passwd
fi

%postun
if [ "$1" -eq 0 ] && [ -f %{_sysconfdir}/pam.d/passwd ]; then
    sed -i '/pam_tessel_keyring\.so/d' %{_sysconfdir}/pam.d/passwd
fi

%files
%{tessel_libdir}/tessel-keyring
%{_bindir}/tessel-keyring
%{tessel_libdir}/tessel-keyring-prompt
%{_bindir}/tessel-keyring-prompt
%{_datadir}/applications/org.tessel.Passwords.desktop
%{_userunitdir}/tessel-keyring.service
%{_datadir}/dbus-1/services/org.freedesktop.secrets.service
%{_datadir}/xdg-desktop-portal/portals/tessel-keyring.portal
%{_libdir}/security/pam_tessel_keyring.so

%changelog
* Wed Nov 11 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-3
- Window buttons are as far from the side of the window as from its top
- Rounded corners are 8 px, as browsers' own windows have

* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- Require tessel-common by version, so it can update on its own

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
