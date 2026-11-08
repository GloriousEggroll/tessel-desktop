# Installs the complete Tessel desktop. It ships no files of its own.
Name:           tessel-meta
Version:        0.1.0
Release:        1%{?dist}
Summary:        The complete Tessel desktop
License:        GPL-3.0-or-later
URL:            https://nobaraproject.org
BuildArch:      noarch

Requires:       tessel-common = %{version}-%{release}
Requires:       tessel-archive = %{version}-%{release}
Requires:       tessel-audio = %{version}-%{release}
Requires:       tessel-auth = %{version}-%{release}
Requires:       tessel-battery = %{version}-%{release}
Requires:       tessel-bluetooth = %{version}-%{release}
Requires:       tessel-brightness = %{version}-%{release}
Requires:       tessel-calculator = %{version}-%{release}
Requires:       tessel-calendar = %{version}-%{release}
Requires:       tessel-capture = %{version}-%{release}
Requires:       tessel-capture-picker = %{version}-%{release}
Requires:       tessel-clipboard = %{version}-%{release}
Requires:       tessel-displays = %{version}-%{release}
Requires:       tessel-dock = %{version}-%{release}
Requires:       tessel-files = %{version}-%{release}
Requires:       tessel-hdr-calibration = %{version}-%{release}
Requires:       tessel-keep-awake = %{version}-%{release}
Requires:       tessel-keyboard = %{version}-%{release}
Requires:       tessel-keyring = %{version}-%{release}
Requires:       tessel-language = %{version}-%{release}
Requires:       tessel-lock = %{version}-%{release}
Requires:       tessel-login = %{version}-%{release}
Requires:       tessel-media = %{version}-%{release}
Requires:       tessel-network = %{version}-%{release}
Requires:       tessel-network-discovery = %{version}-%{release}
Requires:       tessel-notifications = %{version}-%{release}
Requires:       tessel-pictures = %{version}-%{release}
Requires:       tessel-power = %{version}-%{release}
Requires:       tessel-prompt = %{version}-%{release}
Requires:       tessel-session = %{version}-%{release}
Requires:       tessel-session-startup = %{version}-%{release}
Requires:       tessel-settings = %{version}-%{release}
Requires:       tessel-shell = %{version}-%{release}
Requires:       tessel-system-monitor = %{version}-%{release}
Requires:       tessel-text-editor = %{version}-%{release}
Requires:       tessel-tiling = %{version}-%{release}
Requires:       tessel-wayland-helpers = %{version}-%{release}
Requires:       tessel-weather = %{version}-%{release}
Requires:       tessel-welcome = %{version}-%{release}

Requires:       labwc >= 0.20.2-17
Requires:       labwc-session
Requires:       ptyxis
Requires:       xdg-desktop-portal-wlr >= 0.8.4-4
Requires:       xdg-desktop-portal
Requires:       greetd
Requires:       way-displays
Requires:       NetworkManager
Requires:       nm-connection-editor
Requires:       pipewire
Requires:       wireplumber
Requires:       polkit
Requires:       papirus-icon-theme

%description
Installs every part of the Tessel desktop and the programs it runs on: the labwc
compositor, the Ptyxis terminal and the xdg-desktop-portal-wlr screen-share portal.

%files

%changelog
* Sun Nov 08 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
