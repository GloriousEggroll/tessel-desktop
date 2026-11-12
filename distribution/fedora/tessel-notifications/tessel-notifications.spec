# Tessel component "notifications". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

# Sources come from GitHub: the commit this package is built from, and the
# vendored crates for that commit's Cargo.lock (see README.md, "Sources").
# build.sh overrides both to build the working tree.
%{!?commit:%global commit 839b47aed1e9549cd97437533b9965d6cb657d81}
%{!?vendorlock:%global vendorlock ace8d23feae6}

Name:           tessel-notifications
Version:        0.1.0
Release:        4%{?dist}
Summary:        Tessel notifications
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
BuildRequires:  pkgconfig(openssl)
BuildRequires:  pkgconfig(pango)
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  rust
BuildRequires:  systemd-rpm-macros

Requires:       tessel-common >= %{version}
Requires:       libnotify
Requires:       tessel-wayland-helpers

%description
Tessel notifications. Part of the Tessel desktop.

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
cargo build --release --offline --locked -p tessel-notifications

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-notifications" %{buildroot}%{tessel_libdir}/tessel-notifications
ln -s ../lib/tessel/tessel-notifications %{buildroot}%{_bindir}/tessel-notifications
install -m755 "$CARGO_TARGET_DIR/release/tessel-notifications-status" %{buildroot}%{tessel_libdir}/tessel-notifications-status
ln -s ../lib/tessel/tessel-notifications-status %{buildroot}%{_bindir}/tessel-notifications-status
install -d %{buildroot}%{_userunitdir}
sed 's|%h/.local/lib/tessel|%{tessel_libdir}|g' data/units/tessel-notifications.service > %{buildroot}%{_userunitdir}/tessel-notifications.service
chmod 644 %{buildroot}%{_userunitdir}/tessel-notifications.service
install -d %{buildroot}%{_datadir}/applications
sed -e 's|@LIBDIR@|%{tessel_libdir}|g' -e 's#^\(Exec\|TryExec\)=tessel-#\1=%{tessel_libdir}/tessel-#' data/applications/org.tessel.Notifications.desktop > %{buildroot}%{_datadir}/applications/org.tessel.Notifications.desktop
chmod 644 %{buildroot}%{_datadir}/applications/org.tessel.Notifications.desktop

%files
%{tessel_libdir}/tessel-notifications
%{_bindir}/tessel-notifications
%{tessel_libdir}/tessel-notifications-status
%{_bindir}/tessel-notifications-status
%{_userunitdir}/tessel-notifications.service
%{_datadir}/applications/org.tessel.Notifications.desktop

%changelog
* Thu Nov 12 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-4
- Avatars and small or square images are shown as a thumbnail beside the text, and pictures are bounded in height, so popups are no longer oversized
- New setting: Notification size (Small, Medium, Large)

* Wed Nov 11 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-3
- Window buttons are as far from the side of the window as from its top
- Rounded corners are 8 px, as browsers' own windows have

* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- Require tessel-common by version, so it can update on its own

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
