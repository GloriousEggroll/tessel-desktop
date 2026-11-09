# Tessel component "clipboard". Built from one source tree per release: see
# distribution/fedora/README.md for how the sources are made.
%global debug_package %{nil}
%global tessel_libdir %{_prefix}/lib/tessel
%{!?tessel_cargo_target_dir:%global tessel_cargo_target_dir %{_builddir}/tessel-target}

# Sources come from GitHub: the commit this package is built from, and the
# vendored crates for that commit's Cargo.lock (see README.md, "Sources").
# build.sh overrides both to build the working tree.
%{!?commit:%global commit 9e2c93a5ea13b8c2991f561aae0bae7993f2ee6d}
%{!?vendorlock:%global vendorlock ace8d23feae6}

Name:           tessel-clipboard
Version:        0.1.0
Release:        2%{?dist}
Summary:        Tessel clipboard: keeps copied content after its application closes
License:        GPL-3.0-or-later
URL:            https://github.com/GloriousEggroll/tessel-desktop
Source0:        %{url}/archive/%{commit}/tessel-desktop-%{commit}.tar.gz
Source1:        %{url}/releases/download/vendor-%{vendorlock}/tessel-desktop-vendor-%{vendorlock}.tar.xz

BuildRequires:  cargo
BuildRequires:  gcc
BuildRequires:  pkgconfig(gio-2.0)
BuildRequires:  pkgconfig(glib-2.0)
BuildRequires:  pkgconfig(gobject-2.0)
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  rust
BuildRequires:  systemd-rpm-macros

Requires:       tessel-common >= %{version}

%description
Tessel clipboard: keeps copied content after its application closes. Part of the Tessel desktop.

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
cargo build --release --offline --locked -p tessel-clipboard

%install
export CARGO_TARGET_DIR=%{tessel_cargo_target_dir}
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_bindir}
install -m755 "$CARGO_TARGET_DIR/release/tessel-clipboard" %{buildroot}%{tessel_libdir}/tessel-clipboard
ln -s ../lib/tessel/tessel-clipboard %{buildroot}%{_bindir}/tessel-clipboard
install -d %{buildroot}%{_userunitdir}
sed 's|%h/.local/lib/tessel|%{tessel_libdir}|g' data/units/tessel-clipboard.service > %{buildroot}%{_userunitdir}/tessel-clipboard.service
chmod 644 %{buildroot}%{_userunitdir}/tessel-clipboard.service
install -Dm644 packaging/defaults/usr/lib/systemd/user-preset/90-tessel.preset %{buildroot}%{_userpresetdir}/90-tessel.preset

%post
%systemd_user_post tessel-clipboard.service

%preun
%systemd_user_preun tessel-clipboard.service

%files
%{tessel_libdir}/tessel-clipboard
%{_bindir}/tessel-clipboard
%{_userunitdir}/tessel-clipboard.service
%{_userpresetdir}/90-tessel.preset

%changelog
* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- Require tessel-common by version, so it can update on its own

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
