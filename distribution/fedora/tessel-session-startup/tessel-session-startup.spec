# Tessel component "session-startup". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

# Sources come from GitHub: the commit this package is built from, and the
# vendored crates for that commit's Cargo.lock (see README.md, "Sources").
# build.sh overrides both to build the working tree.
%{!?commit:%global commit c876a71db3277fa3c684f31dd789dd3ae4188479}
%{!?vendorlock:%global vendorlock ace8d23feae6}

Name:           tessel-session-startup
Version:        0.1.0
Release:        4%{?dist}
Summary:        Tessel session: labwc configuration, autostart and login entry
License:        GPL-3.0-or-later
URL:            https://github.com/GloriousEggroll/tessel-desktop
Source0:        %{url}/archive/%{commit}/tessel-desktop-%{commit}.tar.gz
Source1:        %{url}/releases/download/vendor-%{vendorlock}/tessel-desktop-vendor-%{vendorlock}.tar.xz

BuildRequires:  cargo
BuildRequires:  gcc
BuildRequires:  pkgconfig(gio-2.0)
BuildRequires:  pkgconfig(glib-2.0)
BuildRequires:  pkgconfig(gobject-2.0)
BuildRequires:  rust
BuildRequires:  systemd-rpm-macros

Requires:       tessel-common >= %{version}
Requires:       labwc >= 0.20.2-17
Requires:       xdg-desktop-portal
Requires:       xdg-desktop-portal-wlr >= 0.8.4-4
Requires:       ptyxis
Requires:       glib2
Requires:       systemd
Recommends:     playerctl
Recommends:     brightnessctl
Recommends:     labwc-session
Recommends:     tessel-meta

%description
Tessel session: labwc configuration, autostart and login entry. Part of the Tessel desktop.

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
cargo build --release --offline --locked -p tessel-session-startup

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-configure-session" %{buildroot}%{tessel_libdir}/tessel-configure-session
ln -s ../lib/tessel/tessel-configure-session %{buildroot}%{_bindir}/tessel-configure-session
install -m755 "$CARGO_TARGET_DIR/release/tessel-failed" %{buildroot}%{tessel_libdir}/tessel-failed
ln -s ../lib/tessel/tessel-failed %{buildroot}%{_bindir}/tessel-failed
install -m755 "$CARGO_TARGET_DIR/release/tessel-icon-theme" %{buildroot}%{tessel_libdir}/tessel-icon-theme
ln -s ../lib/tessel/tessel-icon-theme %{buildroot}%{_bindir}/tessel-icon-theme
install -m755 "$CARGO_TARGET_DIR/release/tessel-migrate" %{buildroot}%{tessel_libdir}/tessel-migrate
ln -s ../lib/tessel/tessel-migrate %{buildroot}%{_bindir}/tessel-migrate
install -m755 "$CARGO_TARGET_DIR/release/tessel-xdg-autostart" %{buildroot}%{tessel_libdir}/tessel-xdg-autostart
ln -s ../lib/tessel/tessel-xdg-autostart %{buildroot}%{_bindir}/tessel-xdg-autostart
install -d %{buildroot}%{_userunitdir}
sed 's|%h/.local/lib/tessel|%{tessel_libdir}|g' data/units/labwc-xdg-autostart.target > %{buildroot}%{_userunitdir}/labwc-xdg-autostart.target
chmod 644 %{buildroot}%{_userunitdir}/labwc-xdg-autostart.target
install -d %{buildroot}%{_userunitdir}
sed 's|%h/.local/lib/tessel|%{tessel_libdir}|g' data/units/tessel-failed@.service > %{buildroot}%{_userunitdir}/tessel-failed@.service
chmod 644 %{buildroot}%{_userunitdir}/tessel-failed@.service
for script in tessel-action tessel-shell-lock; do
    install -Dm755 data/bin/$script %{buildroot}%{tessel_libdir}/$script
    ln -s ../lib/tessel/$script %{buildroot}%{_bindir}/$script
done
install -Dm755 data/session/tessel-session-launch %{buildroot}%{tessel_libdir}/tessel-session-launch
echo '%{version}-%{release}' > %{buildroot}%{tessel_libdir}/session-version
sed 's|^Exec=labwc$|Exec=%{tessel_libdir}/tessel-session-launch|' data/wayland-sessions/tessel.desktop > tessel.desktop
grep -q '^Exec=%{tessel_libdir}/tessel-session-launch$' tessel.desktop
install -Dm644 tessel.desktop %{buildroot}%{_datadir}/wayland-sessions/tessel.desktop
install -Dm644 packaging/defaults/etc/xdg/autostart/tessel-icon-theme.desktop %{buildroot}%{_sysconfdir}/xdg/autostart/tessel-icon-theme.desktop

%files
%{tessel_libdir}/tessel-configure-session
%{_bindir}/tessel-configure-session
%{tessel_libdir}/tessel-failed
%{_bindir}/tessel-failed
%{tessel_libdir}/tessel-icon-theme
%{_bindir}/tessel-icon-theme
%{tessel_libdir}/tessel-migrate
%{_bindir}/tessel-migrate
%{tessel_libdir}/tessel-xdg-autostart
%{_bindir}/tessel-xdg-autostart
%{_userunitdir}/labwc-xdg-autostart.target
%{_userunitdir}/tessel-failed@.service
%{tessel_libdir}/tessel-action
%{tessel_libdir}/tessel-shell-lock
%{_bindir}/tessel-action
%{_bindir}/tessel-shell-lock
%{tessel_libdir}/tessel-session-launch
%{tessel_libdir}/session-version
%{_datadir}/wayland-sessions/tessel.desktop
%config(noreplace) %{_sysconfdir}/xdg/autostart/tessel-icon-theme.desktop

%changelog
* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-4
- Restart a screen-share portal left over from another desktop at login

* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-3
- New users start without window gaps, as the Win layout has

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- A new user gets complete labwc configuration: Alt+Tab, Super, screenshots, dialogs, mouse, menu

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
