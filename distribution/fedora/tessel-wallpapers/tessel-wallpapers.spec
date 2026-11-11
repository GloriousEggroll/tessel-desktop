# The Slate wallpaper (branding/wallpaper), which a screen shows until its
# user chooses another (tessel_core::wallpaper).
#
# The pictures are large, so the archive the other packages are built from
# leaves branding/wallpaper out (.gitattributes). Here each picture is a source
# of its own: the file at this spec's commit. build.sh makes them from the
# local repository.
%{!?commit:%global commit 219a63a92b10f8502a8b97012868f4524014fc05}
%global pictures https://github.com/GloriousEggroll/tessel-desktop/raw/%{commit}/branding/wallpaper/renders

Name:           tessel-wallpapers
Version:        0.1.0
Release:        1%{?dist}
Summary:        Tessel desktop: the Slate wallpaper
License:        GPL-3.0-or-later
URL:            https://github.com/GloriousEggroll/tessel-desktop
Source0:        %{pictures}/slate-3840x2160.png
Source1:        %{pictures}/slate-3840x2400.png
Source2:        %{pictures}/slate-7680x2160.png
Source3:        %{pictures}/slate-light-3840x2160.png
Source4:        %{pictures}/slate-light-3840x2400.png
Source5:        %{pictures}/slate-light-7680x2160.png
BuildArch:      noarch

%description
Slate, the wallpaper of the Tessel desktop: layers of black and dark grey stone,
then tanzanite, with blue zircon at the back. A dark and a light version, each
for 16:9, 16:10 and 32:9 screens. A screen shows the dark one that fits its
shape until its user chooses another wallpaper.

%prep

%build

%install
install -d %{buildroot}%{_datadir}/wallpapers/tessel
install -m644 %{SOURCE0} %{SOURCE1} %{SOURCE2} %{SOURCE3} %{SOURCE4} %{SOURCE5} %{buildroot}%{_datadir}/wallpapers/tessel/

%files
%dir %{_datadir}/wallpapers
%{_datadir}/wallpapers/tessel

%changelog
* Wed Nov 11 2026 GloriousEggroll <tcrider84@gmail.com> - 0.1.0-1
- First package
