# Installs the complete Tessel desktop. It ships no files of its own.
Name:           tessel-meta
Version:        0.1.0
Release:        3%{?dist}
Summary:        The complete Tessel desktop
License:        GPL-3.0-or-later
URL:            https://github.com/GloriousEggroll/tessel-desktop
BuildArch:      noarch

Requires:       tessel-common >= %{version}
Requires:       tessel-archive >= %{version}
Requires:       tessel-audio >= %{version}
Requires:       tessel-auth >= %{version}
Requires:       tessel-battery >= %{version}
Requires:       tessel-bluetooth >= %{version}
Requires:       tessel-brightness >= %{version}
Requires:       tessel-calculator >= %{version}
Requires:       tessel-calendar >= %{version}
Requires:       tessel-capture >= %{version}
Requires:       tessel-capture-picker >= %{version}
Requires:       tessel-clipboard >= %{version}
Requires:       tessel-displays >= %{version}
Requires:       tessel-dock >= %{version}
Requires:       tessel-files >= %{version}
Requires:       tessel-hdr-calibration >= %{version}
Requires:       tessel-keep-awake >= %{version}
Requires:       tessel-keyboard >= %{version}
Requires:       tessel-keyring >= %{version}
Requires:       tessel-language >= %{version}
Requires:       tessel-lock >= %{version}
Requires:       tessel-login >= %{version}
Requires:       tessel-media >= %{version}
Requires:       tessel-network >= %{version}
Requires:       tessel-network-discovery >= %{version}
Requires:       tessel-notifications >= %{version}
Requires:       tessel-pictures >= %{version}
Requires:       tessel-power >= %{version}
Requires:       tessel-prompt >= %{version}
Requires:       tessel-session >= %{version}
Requires:       tessel-session-startup >= %{version}-2
Requires:       tessel-settings >= %{version}
Requires:       tessel-shell >= %{version}
Requires:       tessel-system-monitor >= %{version}
Requires:       tessel-text-editor >= %{version}
Requires:       tessel-tiling >= %{version}
Requires:       tessel-wallpapers >= %{version}
Requires:       tessel-wayland-helpers >= %{version}
Requires:       tessel-weather >= %{version}
Requires:       tessel-welcome >= %{version}

Requires:       labwc >= 0.20.2-17
Requires:       labwc-session
Requires:       ptyxis
Requires:       xdg-desktop-portal-wlr >= 0.8.4-4
Requires:       xdg-desktop-portal
Requires:       greetd
Requires:       NetworkManager
Requires:       nm-connection-editor
Requires:       pipewire
Requires:       wireplumber
Requires:       polkit

%description
Installs every part of the Tessel desktop and the programs it runs on: the labwc
compositor, the Ptyxis terminal and the xdg-desktop-portal-wlr screen-share portal.

%files

%changelog
* Wed Nov 11 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-3
- Require tessel-wallpapers, the default wallpaper

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-2
- Allow Tessel packages to update one at a time

* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
