#!/bin/sh
# Installs fishpr from its signed package repo (pacman on Arch, apt on Ubuntu
# and Debian) or as a signed package (dnf on Fedora), then starts fishpr.
#
#   curl -fsSL https://raw.githubusercontent.com/srafis/fishpr/main/install.sh | sh
#   wget -qO- https://raw.githubusercontent.com/srafis/fishpr/main/install.sh | sh
#
# (Ubuntu and Debian ship wget but not curl.)
#
# Safe to run again. After the first run, the system's normal upgrades
# (`pacman -Syu`, `apt upgrade`) keep fishpr updated. On Fedora, run this again
# to update.

set -eu

RELEASES=https://github.com/srafis/fishpr/releases/download
APP_ID=io.github.srafis.fishpr

die() {
    printf 'fishpr: %s\n' "$*" >&2
    exit 1
}

# Downloads URL $1 to file $2 with whichever of curl and wget is installed.
fetch() {
    if command -v curl >/dev/null; then
        curl -fsSL "$1" -o "$2"
    else
        wget -qO "$2" "$1"
    fi
}

install_pacman() {
    fetch "$RELEASES/repo/fishpr.gpg" "$tmp/fishpr.gpg" || die "couldn't download the signing key"
    fpr=$(gpg --show-keys --with-colons "$tmp/fishpr.gpg" | awk -F: '/^fpr/ { print $10; exit }')
    [ -n "$fpr" ] || die "couldn't read the signing key"

    echo "==> Trusting fishpr's package signing key $fpr"
    sudo pacman-key --add "$tmp/fishpr.gpg" >/dev/null
    sudo pacman-key --lsign-key "$fpr" >/dev/null

    if ! grep -q '^\[fishpr\]' /etc/pacman.conf; then
        echo "==> Adding the [fishpr] repo to /etc/pacman.conf"
        printf '\n[fishpr]\nSigLevel = Required\nServer = %s\n' "$RELEASES/repo" | sudo tee -a /etc/pacman.conf >/dev/null
    fi

    # -Syu rather than -Sy: installing onto an out-of-date system is a partial
    # upgrade.
    echo "==> Installing fishpr-bin (this also upgrades the rest of the system)"
    sudo pacman -Syu --needed fishpr-bin </dev/tty
}

install_apt() {
    fetch "$RELEASES/apt/fishpr.gpg" "$tmp/fishpr.asc" || die "couldn't download the signing key"
    grep -q 'BEGIN PGP PUBLIC KEY BLOCK' "$tmp/fishpr.asc" || die "couldn't read the signing key"

    echo "==> Trusting fishpr's package signing key for fishpr's repo only"
    sudo install -Dm644 "$tmp/fishpr.asc" /etc/apt/keyrings/fishpr.asc

    echo "==> Adding the fishpr repo to /etc/apt/sources.list.d/fishpr.sources"
    printf 'Types: deb\nURIs: %s/apt/\nSuites: ./\nSigned-By: /etc/apt/keyrings/fishpr.asc\n' "$RELEASES" |
        sudo tee /etc/apt/sources.list.d/fishpr.sources >/dev/null

    echo "==> Installing fishpr"
    sudo apt-get update
    sudo apt-get install fishpr </dev/tty
}

install_dnf() {
    fetch "$RELEASES/rpm/fishpr.gpg" "$tmp/fishpr.gpg" || die "couldn't download the signing key"
    fetch "$RELEASES/rpm/fishpr.x86_64.rpm" "$tmp/fishpr.rpm" || die "couldn't download the package"

    # Checked against a throwaway key database, so the key isn't trusted for anything else.
    echo "==> Checking the package signature"
    mkdir "$tmp/rpmdb"
    rpmkeys --dbpath "$tmp/rpmdb" --import "$tmp/fishpr.gpg" || die "couldn't read the signing key"
    rpmkeys --dbpath "$tmp/rpmdb" --checksig "$tmp/fishpr.rpm" | grep -q 'signatures OK' ||
        die "the package's signature doesn't check out"

    echo "==> Installing fishpr"
    sudo dnf install "$tmp/fishpr.rpm" </dev/tty
}

main() {
    [ "$(id -u)" -ne 0 ] || die "run this as your normal user; it asks for sudo when it needs it"
    [ "$(uname -m)" = x86_64 ] || die "only x86_64 builds are published"
    if command -v pacman >/dev/null; then
        pm=pacman
    elif command -v apt-get >/dev/null; then
        pm=apt
    elif command -v dnf >/dev/null; then
        pm=dnf
    else
        die "there's no fishpr package for this distro yet (Arch, Ubuntu, Debian, and Fedora are supported); see https://github.com/srafis/fishpr#build-from-source"
    fi

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT

    "install_$pm"

    # Restart a running copy so an update takes effect right away. SIGTERM makes
    # fishpr release its shortcut before it exits.
    if pkill -x fishpr; then
        i=0
        while pgrep -x fishpr >/dev/null && [ "$i" -lt 50 ]; do
            sleep 0.1
            i=$((i + 1))
        done
    fi
    if systemd-run --user --unit="app-$APP_ID" --collect /usr/bin/fishpr >/dev/null 2>&1; then
        echo "==> fishpr is running. Hold Ctrl+Space to dictate."
    else
        echo "==> Installed. Start fishpr from the app menu, or log out and back in."
    fi
}

# Everything runs from here, so a partial download can't run half a script.
main "$@"
