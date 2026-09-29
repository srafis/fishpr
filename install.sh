#!/bin/sh
# Installs fishpr from its signed pacman repo, then starts it.
#
#   curl -fsSL https://raw.githubusercontent.com/srafis/fishpr/main/install.sh | sh
#
# Safe to run again. After the first run, `pacman -Syu` (or `yay`) keeps fishpr
# updated along with the rest of the system.

set -eu

REPO_URL=https://github.com/srafis/fishpr/releases/download/repo

die() {
    printf 'fishpr: %s\n' "$*" >&2
    exit 1
}

main() {
    [ "$(id -u)" -ne 0 ] || die "run this as your normal user; it asks for sudo when it needs it"
    command -v pacman >/dev/null || die "this installer is for Arch Linux (pacman not found)"
    [ "$(uname -m)" = x86_64 ] || die "only x86_64 builds are published"

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT

    curl -fsSL "$REPO_URL/fishpr.gpg" -o "$tmp/fishpr.gpg" || die "couldn't download the signing key"
    fpr=$(gpg --show-keys --with-colons "$tmp/fishpr.gpg" | awk -F: '/^fpr/ { print $10; exit }')
    [ -n "$fpr" ] || die "couldn't read the signing key"

    echo "==> Trusting fishpr's package signing key $fpr"
    sudo pacman-key --add "$tmp/fishpr.gpg" >/dev/null
    sudo pacman-key --lsign-key "$fpr" >/dev/null

    if ! grep -q '^\[fishpr\]' /etc/pacman.conf; then
        echo "==> Adding the [fishpr] repo to /etc/pacman.conf"
        printf '\n[fishpr]\nSigLevel = Required\nServer = %s\n' "$REPO_URL" | sudo tee -a /etc/pacman.conf >/dev/null
    fi

    # -Syu rather than -Sy: installing onto an out-of-date system is a partial
    # upgrade. stdin is this script when piped from curl, so prompts read the tty.
    echo "==> Installing fishpr-bin (this also upgrades the rest of the system)"
    sudo pacman -Syu --needed fishpr-bin </dev/tty

    # Restart a running copy so an update takes effect right away. SIGTERM makes
    # fishpr release Ctrl+Space before it exits.
    if pkill -x fishpr; then
        i=0
        while pgrep -x fishpr >/dev/null && [ "$i" -lt 50 ]; do
            sleep 0.1
            i=$((i + 1))
        done
    fi
    if systemd-run --user --unit=app-fishpr --collect /usr/bin/fishpr >/dev/null 2>&1; then
        echo "==> fishpr is running. Hold Ctrl+Space to dictate."
    else
        echo "==> Installed. Start fishpr from the app menu, or log out and back in."
    fi
}

# Everything runs from here, so a partial download can't run half a script.
main "$@"
