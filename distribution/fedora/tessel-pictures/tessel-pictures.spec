# Tessel component "pictures". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

# Sources come from GitHub: the commit this package is built from, and the
# vendored crates for that commit's Cargo.lock (see README.md, "Sources").
# build.sh overrides both to build the working tree.
%{!?commit:%global commit 50c5cb449e68572c65d21b53ca8f5cbb766174f4}
%{!?vendorlock:%global vendorlock ace8d23feae6}

Name:           tessel-pictures
Version:        0.1.0
Release:        4%{?dist}
Summary:        Tessel picture viewer
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

Requires:       tessel-common >= %{version}
Requires:       lcms2

%description
Tessel picture viewer. Part of the Tessel desktop.

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
cargo build --release --offline --locked -p tessel-pictures

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-pictures" %{buildroot}%{tessel_libdir}/tessel-pictures
ln -s ../lib/tessel/tessel-pictures %{buildroot}%{_bindir}/tessel-pictures
install -d %{buildroot}%{_datadir}/applications
sed -e 's|@LIBDIR@|%{tessel_libdir}|g' -e 's#^\(Exec\|TryExec\)=tessel-#\1=%{tessel_libdir}/tessel-#' components/pictures/data/org.tessel.Pictures.desktop > %{buildroot}%{_datadir}/applications/org.tessel.Pictures.desktop
chmod 644 %{buildroot}%{_datadir}/applications/org.tessel.Pictures.desktop

%files
%{tessel_libdir}/tessel-pictures
%{_bindir}/tessel-pictures
%{_datadir}/applications/org.tessel.Pictures.desktop

%changelog
* Fri Nov 13 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-4
- A large video in a small window is drawn without GTK's full-size copy of each frame (issue 17)

* Wed Nov 11 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-3
- Window buttons are as far from the side of the window as from its top
- Rounded corners are 8 px, as browsers' own windows have

* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- Require tessel-common by version, so it can update on its own

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
