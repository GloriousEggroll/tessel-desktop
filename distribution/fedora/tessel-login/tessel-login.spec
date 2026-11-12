# Tessel component "login". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

# Sources come from GitHub: the commit this package is built from, and the
# vendored crates for that commit's Cargo.lock (see README.md, "Sources").
# build.sh overrides both to build the working tree.
%{!?commit:%global commit b733a1f7039557730d0d310e6c18b6a9a836a38c}
%{!?vendorlock:%global vendorlock ace8d23feae6}

Name:           tessel-login
Version:        0.1.0
Release:        5%{?dist}
Summary:        Tessel login screen (greetd greeter)
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
BuildRequires:  pkgconfig(libudev)
BuildRequires:  pkgconfig(openssl)
BuildRequires:  pkgconfig(pango)
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  pkgconfig(wayland-scanner)
BuildRequires:  pkgconfig(xkbcommon)
BuildRequires:  rust
BuildRequires:  systemd-rpm-macros

Requires:       tessel-common >= %{version}
Requires:       greetd
Requires:       labwc >= 0.20.2-17
Requires:       /usr/bin/dbus-run-session
Requires:       pam
Requires(post): coreutils
Requires(post): systemd
Requires:       tessel-keyboard
Requires:       tessel-wallpapers
Recommends:     at-spi2-core
Recommends:     orca

%description
Tessel login screen (greetd greeter). Part of the Tessel desktop.

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
cargo build --release --offline --locked -p tessel-login

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-login" %{buildroot}%{tessel_libdir}/tessel-login
install -m755 "$CARGO_TARGET_DIR/release/tessel-login-helper" %{buildroot}%{tessel_libdir}/tessel-login-helper
install -m755 "$CARGO_TARGET_DIR/release/tessel-session-transition" %{buildroot}%{tessel_libdir}/tessel-session-transition
for script in tessel-start-greeter tessel-start-session; do
    sed 's|/usr/local/lib/tessel|%{tessel_libdir}|g' data/login/$script > $script
    install -Dm755 $script %{buildroot}%{tessel_libdir}/$script
done
install -Dm644 packaging/defaults/etc/pam.d/tessel-login %{buildroot}%{_sysconfdir}/pam.d/tessel-login
install -Dm644 packaging/defaults/etc/greetd/tessel-config.toml %{buildroot}%{_sysconfdir}/greetd/tessel-config.toml
install -Dm644 packaging/defaults/etc/tessel/login/appearance.json %{buildroot}%{_sysconfdir}/tessel/login/appearance.json
install -Dm644 packaging/defaults/etc/tessel/login/labwc/rc.xml %{buildroot}%{_sysconfdir}/tessel/login/labwc/rc.xml
install -Dm755 packaging/defaults/etc/tessel/login/labwc/autostart %{buildroot}%{_sysconfdir}/tessel/login/labwc/autostart
install -Dm644 packaging/defaults/usr/lib/systemd/system/tessel-login.service %{buildroot}%{_unitdir}/tessel-login.service

%post
# First install only, so a user's later choice is never undone by an update.
# Takes over from greetd, whose own service and configuration stay untouched.
# Another display manager is left alone. The running login is never restarted.
if [ "$1" -eq 1 ]; then
    manager=$(readlink /etc/systemd/system/display-manager.service 2>/dev/null) || manager=
    case "${manager##*/}" in
        ""|greetd.service)
            systemctl --no-reload disable greetd.service >/dev/null 2>&1 || :
            systemctl --no-reload enable --force tessel-login.service >/dev/null 2>&1 || :
            ;;
        *)
            echo "tessel-login: keeping ${manager##*/}. To use the Tessel login screen: systemctl enable --force tessel-login.service"
            ;;
    esac
fi

%preun
# Never stopped here: stopping the login screen would end every open session.
if [ "$1" -eq 0 ]; then
    systemctl --no-reload disable tessel-login.service >/dev/null 2>&1 || :
fi

%files
%{tessel_libdir}/tessel-login
%{tessel_libdir}/tessel-login-helper
%{tessel_libdir}/tessel-session-transition
%{tessel_libdir}/tessel-start-greeter
%{tessel_libdir}/tessel-start-session
%config(noreplace) %{_sysconfdir}/pam.d/tessel-login
%dir %{_sysconfdir}/tessel
%dir %{_sysconfdir}/tessel/login
%dir %{_sysconfdir}/tessel/login/labwc
%config(noreplace) %{_sysconfdir}/greetd/tessel-config.toml
%config(noreplace) %{_sysconfdir}/tessel/login/appearance.json
%config(noreplace) %{_sysconfdir}/tessel/login/labwc/rc.xml
%config(noreplace) %{_sysconfdir}/tessel/login/labwc/autostart
%{_unitdir}/tessel-login.service

%changelog
* Wed Nov 11 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-5
- The login screen's compositor and greeter write to the journal (identifier
  tessel-login-greeter), so a login screen that does not appear can be diagnosed

* Wed Nov 11 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-4
- The default background is the dark Slate wallpaper, the desktop's default
- Each screen shows the render made for its shape
- Require tessel-wallpapers

* Tue Nov 10 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-3
- Send the compositor's output to the journal (identifier tessel-session)

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- Use /etc/greetd/tessel-config.toml, separate from greetd's own configuration
- Enable the login screen on first install, unless another display manager is in use

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
