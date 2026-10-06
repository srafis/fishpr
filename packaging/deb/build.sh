#!/bin/sh
# Builds fishpr_<version>_amd64.deb from an unpacked release tarball, laid out
# like the Arch package. Run by .github/workflows/release.yml.
#
#   packaging/deb/build.sh <version> <unpacked tarball dir> <output dir>

set -eu

version=$1
src=$2
out=$3
here=$(dirname "$0")

root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT

install -Dm755 "$src/fishpr" -t "$root/usr/bin"
install -Dm644 "$src/io.github.srafis.fishpr.desktop" -t "$root/usr/share/applications"
install -Dm644 "$src/io.github.srafis.fishpr.desktop" -t "$root/etc/xdg/autostart"
install -Dm644 "$src/60-fishpr-uinput.rules" -t "$root/usr/lib/udev/rules.d"
install -Dm644 "$src/fishpr.png" -t "$root/usr/share/pixmaps"
mkdir -p "$root/usr/share/doc/fishpr"
cat "$src/LICENSE" "$src/NotoSans-OFL.txt" > "$root/usr/share/doc/fishpr/copyright"

# The packages that provide the libraries the binary links (glibc, Qt), at
# the versions it needs. dpkg-shlibdeps wants to run in a source package.
shlibs=$(mktemp -d)
mkdir "$shlibs/debian"
printf 'Source: fishpr\n\nPackage: fishpr\nArchitecture: amd64\n' > "$shlibs/debian/control"
# Ubuntu calls some of them libfoo6t64 where Debian has libfoo6, so either will do.
shlibdeps=$(cd "$shlibs" && dpkg-shlibdeps -O "$src/fishpr" | sed -n 's/^shlibs:Depends=//p' |
    sed -E 's/(lib[a-z0-9.+-]+)t64( \([^)]*\))?/\1t64\2 | \1\2/g')
rm -r "$shlibs"

mkdir "$root/DEBIAN"
sed -e "s/@VERSION@/$version/" -e "s/@SHLIBDEPS@/$shlibdeps/" \
    -e "s/@SIZE@/$(du -sk --exclude=DEBIAN "$root" | cut -f1)/" "$here/control" > "$root/DEBIAN/control"
install -m755 "$here/postinst" "$root/DEBIAN/postinst"
echo /etc/xdg/autostart/io.github.srafis.fishpr.desktop > "$root/DEBIAN/conffiles"

mkdir -p "$out"
dpkg-deb --root-owner-group --build "$root" "$out/fishpr_${version}_amd64.deb"
