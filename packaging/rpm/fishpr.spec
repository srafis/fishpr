# Packages an already-built fishpr, laid out like the Arch and Debian packages.
# Run by .github/workflows/release.yml:
#
#   rpmbuild -bb --define "pkgver <version>" --define "stage <dir with the built files>" fishpr.spec

Name:           fishpr
Version:        %{pkgver}
Release:        1
Summary:        Push-to-talk dictation for KDE Plasma
License:        MIT AND OFL-1.1
URL:            https://github.com/srafis/fishpr
ExclusiveArch:  x86_64

Requires:       coreutils
Requires:       pipewire-utils
Requires:       wl-clipboard
Requires:       libnotify
Recommends:     xclip

# The release profile already strips the binary.
%global debug_package %{nil}

%description
Hold Ctrl+Space, speak, release, and the text is typed where your cursor is.

%install
install -Dm755 %{stage}/fishpr -t %{buildroot}%{_bindir}
install -Dm644 %{stage}/io.github.srafis.fishpr.desktop -t %{buildroot}%{_datadir}/applications
install -Dm644 %{stage}/io.github.srafis.fishpr.desktop -t %{buildroot}%{_sysconfdir}/xdg/autostart
install -Dm644 %{stage}/60-fishpr-uinput.rules -t %{buildroot}/usr/lib/udev/rules.d
install -Dm644 %{stage}/fishpr.png -t %{buildroot}%{_datadir}/pixmaps
install -Dm644 %{stage}/LICENSE %{stage}/NotoSans-OFL.txt -t %{buildroot}%{_licensedir}/%{name}

%post
# Apply the uinput rule now, so silent pasting works without logging out.
udevadm control --reload 2>/dev/null || :
udevadm trigger --action=change --name-match=uinput 2>/dev/null || :

%files
%license %{_licensedir}/%{name}/LICENSE
%license %{_licensedir}/%{name}/NotoSans-OFL.txt
%{_bindir}/fishpr
%{_datadir}/applications/io.github.srafis.fishpr.desktop
%config(noreplace) %{_sysconfdir}/xdg/autostart/io.github.srafis.fishpr.desktop
/usr/lib/udev/rules.d/60-fishpr-uinput.rules
%{_datadir}/pixmaps/fishpr.png
