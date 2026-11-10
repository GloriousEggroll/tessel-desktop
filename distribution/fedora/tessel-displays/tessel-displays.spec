# Tessel component "displays". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

# Sources come from GitHub: the commit this package is built from, and the
# vendored crates for that commit's Cargo.lock (see README.md, "Sources").
# build.sh overrides both to build the working tree.
%{!?commit:%global commit c876a71db3277fa3c684f31dd789dd3ae4188479}
%{!?vendorlock:%global vendorlock ace8d23feae6}

Name:           tessel-displays
Version:        0.1.0
Release:        6%{?dist}
Summary:        Tessel display settings and layout daemon
License:        GPL-3.0-or-later
URL:            https://github.com/GloriousEggroll/tessel-desktop
Source0:        %{url}/archive/%{commit}/tessel-desktop-%{commit}.tar.gz
Source1:        %{url}/releases/download/vendor-%{vendorlock}/tessel-desktop-vendor-%{vendorlock}.tar.xz

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

Requires:       tessel-common >= %{version}
Requires:       wlr-randr
Requires:       libnotify
Requires:       lcms2
Recommends:     tessel-hdr-calibration

%description
Tessel display settings and the daemon that applies them. Part of the Tessel desktop.

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
cargo build --release --offline --locked -p tessel-displays

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-displays" %{buildroot}%{tessel_libdir}/tessel-displays
ln -s ../lib/tessel/tessel-displays %{buildroot}%{_bindir}/tessel-displays
install -d %{buildroot}%{_userunitdir}
sed 's|%h/.local/lib/tessel|%{tessel_libdir}|g' data/units/tessel-displays.service > %{buildroot}%{_userunitdir}/tessel-displays.service
chmod 644 %{buildroot}%{_userunitdir}/tessel-displays.service
install -d %{buildroot}%{_datadir}/applications
sed -e 's|@LIBDIR@|%{tessel_libdir}|g' -e 's#^\(Exec\|TryExec\)=tessel-#\1=%{tessel_libdir}/tessel-#' data/applications/org.tessel.Displays.desktop > %{buildroot}%{_datadir}/applications/org.tessel.Displays.desktop
chmod 644 %{buildroot}%{_datadir}/applications/org.tessel.Displays.desktop
install -Dm644 packaging/defaults/etc/tessel/displays/cfg.yaml %{buildroot}%{_sysconfdir}/tessel/displays/cfg.yaml

%files
%dir %{_sysconfdir}/tessel/displays
%config(noreplace) %{_sysconfdir}/tessel/displays/cfg.yaml
%{_userunitdir}/tessel-displays.service
%{tessel_libdir}/tessel-displays
%{_bindir}/tessel-displays
%{_datadir}/applications/org.tessel.Displays.desktop

%changelog
* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-6
- Keep a turned display's turned size in the layout preview, also after Apply

* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-5
- Ship Tessel's own display defaults instead of relying on way-displays' file

* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-4
- Automatic scale suits the screen size instead of making laptop panels far too large

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-3
- Apply asks for 10 seconds whether to keep display settings, and reverts them otherwise

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- Saving in Global Settings no longer turns off a laptop's internal screen

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
