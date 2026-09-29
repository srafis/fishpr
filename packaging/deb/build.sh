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

# The newest glibc symbol version the binary uses is the oldest glibc it runs on.
glibc=$(objdump -T "$src/fishpr" | grep -o 'GLIBC_[0-9.]*' | cut -d_ -f2 | sort -uV | tail -n1)

mkdir "$root/DEBIAN"
sed -e "s/@VERSION@/$version/" -e "s/@GLIBC@/$glibc/" \
    -e "s/@SIZE@/$(du -sk --exclude=DEBIAN "$root" | cut -f1)/" "$here/control" > "$root/DEBIAN/control"
install -m755 "$here/postinst" "$root/DEBIAN/postinst"
echo /etc/xdg/autostart/io.github.srafis.fishpr.desktop > "$root/DEBIAN/conffiles"

mkdir -p "$out"
dpkg-deb --root-owner-group --build "$root" "$out/fishpr_${version}_amd64.deb"
