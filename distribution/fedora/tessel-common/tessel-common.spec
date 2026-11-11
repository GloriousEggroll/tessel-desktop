# Files every Tessel component shares: translations, the logo, the program
# directory and the system defaults (packaging/defaults/README.md).
%global tessel_libdir %{_prefix}/lib/tessel

# Sources come from GitHub: the commit this package is built from, and the
# vendored crates for that commit's Cargo.lock (see README.md, "Sources").
# build.sh overrides both to build the working tree.
%{!?commit:%global commit 219a63a92b10f8502a8b97012868f4524014fc05}
%{!?vendorlock:%global vendorlock ace8d23feae6}

Name:           tessel-common
Version:        0.1.0
Release:        5%{?dist}
Summary:        Tessel desktop: translations, logo and system defaults
License:        GPL-3.0-or-later
URL:            https://github.com/GloriousEggroll/tessel-desktop
Source0:        %{url}/archive/%{commit}/tessel-desktop-%{commit}.tar.gz
BuildArch:      noarch

Requires:       glib2
Requires:       hicolor-icon-theme
Requires:       papirus-icon-theme-dark
Requires:       papirus-icon-theme-light
Requires:       wlrctl

%description
Translations, the logo, the program directory and the system defaults shared by
every part of the Tessel desktop.

%prep
%autosetup -n tessel-desktop-%{commit}

%build

%install
D=packaging/defaults
install -d %{buildroot}%{tessel_libdir} %{buildroot}%{_datadir}/tessel/locale
install -m644 i18n/locale/*.json %{buildroot}%{_datadir}/tessel/locale/
install -Dm644 branding/tessel-logo.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/tessel.svg
install -Dm644 branding/tessel-symbolic.svg %{buildroot}%{_datadir}/icons/hicolor/symbolic/apps/tessel-symbolic.svg
install -Dm644 $D/usr/share/glib-2.0/schemas/90_tessel.gschema.override %{buildroot}%{_datadir}/glib-2.0/schemas/90_tessel.gschema.override
install -Dm644 $D/usr/share/applications/labwc-mimeapps.list %{buildroot}%{_datadir}/applications/labwc-mimeapps.list
install -Dm644 $D/etc/gtk-3.0/settings.ini %{buildroot}%{_sysconfdir}/gtk-3.0/settings.ini
install -Dm644 $D/etc/gtk-4.0/settings.ini %{buildroot}%{_sysconfdir}/gtk-4.0/settings.ini
install -Dm644 $D/etc/pam.d/tessel-login-auth %{buildroot}%{_sysconfdir}/pam.d/tessel-login-auth
install -Dm644 $D/etc/security/tessel-login-faillock.conf %{buildroot}%{_sysconfdir}/security/tessel-login-faillock.conf

%posttrans
glib-compile-schemas %{_datadir}/glib-2.0/schemas >/dev/null 2>&1 || :

%files
%dir %{tessel_libdir}
%dir %{_datadir}/tessel
%{_datadir}/tessel/locale
%{_datadir}/icons/hicolor/scalable/apps/tessel.svg
%{_datadir}/icons/hicolor/symbolic/apps/tessel-symbolic.svg
%{_datadir}/glib-2.0/schemas/90_tessel.gschema.override
%{_datadir}/applications/labwc-mimeapps.list
%config(noreplace) %{_sysconfdir}/gtk-3.0/settings.ini
%config(noreplace) %{_sysconfdir}/gtk-4.0/settings.ini
%config(noreplace) %{_sysconfdir}/pam.d/tessel-login-auth
%config(noreplace) %{_sysconfdir}/security/tessel-login-faillock.conf

%changelog
* Wed Nov 11 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-5
- Dark style by default

* Tue Nov 10 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-4
- Translations for the bar islands option

* Tue Nov 10 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-3
- Translations for dragging the gap between tiles

* Mon Nov 09 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- Translations for the display settings confirmation

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
