#!/bin/sh
# Installs fishpr from its signed package repo (pacman on Arch, apt on Ubuntu
# and Debian), sets up GNOME if that's the desktop, then starts fishpr.
#
#   curl -fsSL https://raw.githubusercontent.com/srafis/fishpr/main/install.sh | sh
#   wget -qO- https://raw.githubusercontent.com/srafis/fishpr/main/install.sh | sh
#
# (Ubuntu and Debian ship wget but not curl.)
#
# Safe to run again. After the first run, the system's normal upgrades
# (`pacman -Syu`, `apt upgrade`) keep fishpr updated.

set -eu

RELEASES=https://github.com/srafis/fishpr/releases/download
APP_ID=io.github.srafis.fishpr
TOGGLE_KEYBINDING=/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/fishpr/

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

# Asks on the terminal, since stdin is this script when piped from curl.
# Enter means yes; no terminal means no.
ask() {
    printf '%s [Y/n] ' "$1" 2>/dev/null >/dev/tty || return 1
    read -r answer </dev/tty || return 1
    case $answer in [nN]*) return 1 ;; esac
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

install_package() {
    case $pm in
        pacman) sudo pacman -S --needed "$@" </dev/tty ;;
        apt) sudo apt-get install "$@" </dev/tty ;;
    esac
}

# Appends $3 to the GSettings string list at schema $1, key $2, if missing.
gsettings_add() {
    current=$(gsettings get "$1" "$2")
    case $current in
        *"'$3'"*) return 0 ;;
        "@as []" | "[]") new="['$3']" ;;
        *) new="${current%]}, '$3']" ;;
    esac
    gsettings set "$1" "$2" "$new"
}

# GNOME has no tray of its own; the AppIndicator extension adds one. Ubuntu
# ships and enables its own copy, so this usually has nothing to do there.
setup_gnome_tray() {
    command -v gnome-extensions >/dev/null || return 0
    gnome-extensions list --enabled 2>/dev/null | grep -qi appindicator && return 0

    extension=$(gnome-extensions list 2>/dev/null | grep -i appindicator | head -n1)
    if [ -z "$extension" ]; then
        ask "fishpr's tray icon needs GNOME's AppIndicator extension. Install and turn it on?" || return 0
        install_package gnome-shell-extension-appindicator
        extension=appindicatorsupport@rgcjonas.gmail.com
        # GNOME Shell only notices a newly installed extension after a new login.
        relogin=1
    else
        ask "fishpr's tray icon needs GNOME's AppIndicator extension, which is installed but off. Turn it on?" || return 0
    fi
    gsettings_add org.gnome.shell enabled-extensions "$extension"
}

# Whether this session runs GNOME. The calling shell's XDG_CURRENT_DESKTOP can
# be missing (IDE terminals, tmux, ssh), so also ask the user's systemd, which
# GNOME tells, and look for gnome-shell itself.
is_gnome() {
    desktop=${XDG_CURRENT_DESKTOP:-$(systemctl --user show-environment 2>/dev/null | sed -n 's/^XDG_CURRENT_DESKTOP=//p')}
    case ":$desktop:" in *:GNOME:*) return 0 ;; esac
    pgrep -u "$(id -u)" -x gnome-shell >/dev/null
}

# GNOME 48 and later let fishpr register a hold-to-talk shortcut itself (GNOME
# asks the user on first start). Older GNOME can't, so offer a toggle key.
setup_gnome_shortcut() {
    [ "$gnome_version" -ge 48 ] && return 0
    ask "GNOME $gnome_version can't give fishpr a hold-to-talk shortcut. Make Ctrl+Space start and stop dictation instead?" || return 0

    schema=org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:$TOGGLE_KEYBINDING
    gsettings set "$schema" name 'fishpr: start or stop dictation'
    gsettings set "$schema" command 'fishpr --toggle'
    gsettings set "$schema" binding '<Control>space'
    gsettings_add org.gnome.settings-daemon.plugins.media-keys custom-keybindings "$TOGGLE_KEYBINDING"
    toggle_key=1
}

main() {
    [ "$(id -u)" -ne 0 ] || die "run this as your normal user; it asks for sudo when it needs it"
    [ "$(uname -m)" = x86_64 ] || die "only x86_64 builds are published"
    if command -v pacman >/dev/null; then
        pm=pacman
    elif command -v apt-get >/dev/null; then
        pm=apt
    else
        die "there's no fishpr package for this distro yet (Arch, Ubuntu, and Debian are supported); see https://github.com/srafis/fishpr#build-from-source"
    fi

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    relogin=
    toggle_key=

    "install_$pm"

    gnome=
    gnome_version=0
    if is_gnome && command -v gsettings >/dev/null; then
        gnome=1
        gnome_version=$(gnome-shell --version 2>/dev/null | awk '{ print int($3) }')
        setup_gnome_tray
        setup_gnome_shortcut
    fi

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
        if [ -n "$toggle_key" ]; then
            echo "==> fishpr is running. Press Ctrl+Space to start dictating, and again to stop."
        elif [ -n "$gnome" ] && [ "$gnome_version" -lt 48 ]; then
            echo "==> fishpr is running. To dictate, click its tray icon, or bind the command 'fishpr --toggle'"
            echo "    to a key in Settings > Keyboard > Custom Shortcuts."
        elif [ -n "$gnome" ]; then
            echo "==> fishpr is running. Allow its shortcut when GNOME asks, then hold Ctrl+Space to dictate."
        else
            echo "==> fishpr is running. Hold Ctrl+Space to dictate."
        fi
    else
        echo "==> Installed. Start fishpr from the app menu, or log out and back in."
    fi
    if [ -n "$relogin" ]; then
        echo "==> Log out and back in to see fishpr's tray icon."
    fi
}

# Everything runs from here, so a partial download can't run half a script.
main "$@"
