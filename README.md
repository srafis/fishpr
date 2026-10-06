<p align="center">
  <img src="assets/icon.png" width="160" alt="fishpr">
</p>

<h1 align="center">fishpr</h1>

<p align="center">
  Push-to-talk dictation for KDE Plasma on Linux.<br>
  Hold <kbd>Ctrl</kbd>+<kbd>Space</kbd>, speak, release, and the text is typed where your cursor is.
</p>

---

## What it does

- **Hold to talk.** Hold <kbd>Ctrl</kbd>+<kbd>Space</kbd> anywhere. A small black pill appears at the bottom of the screen, its bars moving with your voice. Let go and the transcription lands in the focused window about half a second later, however long you spoke.
- **Or click the tray icon.** Click once to start, click again to stop. Tray and shortcut share one state, so you can start with one and stop with the other.
- **Pastes for you, and leaves your clipboard alone.** The text is pasted into the focused app with <kbd>Ctrl</kbd>+<kbd>V</kbd>, and whatever you had copied is still on your clipboard afterwards. Klipper doesn't keep a copy of the text either.
- **Shows it when there's nowhere to paste.** If no text field takes the paste, the pill grows into a card that shows what you said, with a **Copy** button, for ten seconds. Resting the pointer on it keeps it open.
- **Paste it again.** <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>V</kbd> pastes your last transcription again, wherever the cursor is now.
- **Esc to cancel.** While the pill shows, <kbd>Esc</kbd> cancels the recording or transcription, and the pill offers a retry for three seconds in case you didn't mean it. On a retry button or the card, <kbd>Esc</kbd> hides it. Only fishpr sees that <kbd>Esc</kbd>, not the app you're in; the rest of the time, <kbd>Esc</kbd> works as usual.
- **History.** Everything you dictate is kept, with its recording. Right-click the tray icon → **Open fishpr** to see it by day, copy any of it again, play the recording, save it elsewhere, or delete it. On the entry you've moved to with the arrow keys, <kbd>Space</kbd> plays it, <kbd>Ctrl</kbd>+<kbd>C</kbd> copies it, <kbd>Ctrl</kbd>+<kbd>S</kbd> saves the recording, <kbd>Enter</kbd> shows it in its folder, and <kbd>Del</kbd> deletes it. It all stays on your computer, in `~/.local/share/fishpr/history/`.
- **Retry when it fails.** If transcription fails, the pill stays for ten seconds with a retry button, and a click sends the same recording again. Nothing you said is lost to a network hiccup.
- **Ignores silence.** A local voice check runs on every recording, so an accidental press with nobody speaking pastes nothing, instead of a made-up "Thank you." The pill says "No speech detected" and offers a retry, which skips the voice check in case it was wrong.

## How it works

While you hold the key, the audio streams to Google's speech service, the one behind the microphone button on gemini.google.com. It transcribes as you talk, so when you let go, the text is ready in about half a second. When you release the key, a local voice check also runs on the recording, and if nobody spoke, the result is thrown away. No account or API key is needed.

```
pw-record ──PCM──▶ fishpr ──streams while you talk──▶ Google speech service
    │                │                                         │
  mic          on release: local VAD ── no speech? ──▶ drop    │
                     │                                         │
                  speech ◀──────────── text ◀──────────────────┘
                     │
                     └──▶ Ctrl+V ── nothing took it? ──▶ card with a Copy button
```

Ctrl+V pastes whatever is on the clipboard, so the text sits there for the moment it takes the app to fetch it. Before that, fishpr saves what the clipboard holds, and right after, it puts it back. The text is marked as a secret, the way password managers mark passwords, so Klipper keeps no history of it. If no app fetches the text within a second, nothing took the paste, and the card shows the text instead.

> [!WARNING]
> **This uses an unofficial, undocumented endpoint.** Google can change or block it at any time without notice. Your audio is sent to Google as you speak, including on an accidental press where nobody talks, so don't dictate anything you wouldn't say to Gemini.

## Requirements

- **KDE Plasma 6, on Wayland.** GNOME isn't supported.
- **Arch Linux, Ubuntu 24.04 or later, Debian 13 or later, or Fedora 42 or later, on x86_64** for the one-line installer. On anything else, [build from source](#build-from-source).

## Install

```sh
curl -fsSL https://fishpr.s.gy | sh
```

The installer adds fishpr's signed package repo and installs fishpr from it:

- **Arch:** adds the `[fishpr]` repo to `/etc/pacman.conf` and installs `fishpr-bin`. It uses `pacman -Syu`, so it also upgrades the rest of your system.
- **Ubuntu and Debian:** adds `/etc/apt/sources.list.d/fishpr.sources`, with the signing key in `/etc/apt/keyrings/fishpr.asc` (trusted for fishpr's repo only), and installs `fishpr`.
- **Fedora:** downloads the signed `fishpr` RPM, checks its signature against fishpr's key (which isn't added to your system's keys), and installs it with `dnf`. There's no dnf repo, so nothing is added to your system's sources.

The package brings in the dependencies (PipeWire, wl-clipboard, libnotify), installs the [uinput rule](#pasting) for silent pasting, and makes fishpr start on login. The installer then starts fishpr.

If you installed fishpr by hand before, delete `~/.local/bin/fishpr` and `~/.config/autostart/fishpr.desktop`. The old autostart entry would otherwise override the packaged one.

### Updating

New versions arrive with your normal system upgrade (`sudo pacman -Syu` or `yay` on Arch, `sudo apt upgrade` on Ubuntu and Debian). The running copy keeps its old version until you restart it: run `pkill -x fishpr`, then launch fishpr from the app menu. Or run the install command again, which updates and restarts it in one go. On Fedora that's the only way to update, since fishpr isn't in a dnf repo.

### Uninstall

On Arch:

```sh
sudo pacman -R fishpr-bin
```

Then delete the `[fishpr]` section at the end of `/etc/pacman.conf`.

On Ubuntu and Debian:

```sh
sudo apt purge fishpr
sudo rm /etc/apt/sources.list.d/fishpr.sources /etc/apt/keyrings/fishpr.asc
```

On Fedora:

```sh
sudo dnf remove fishpr
```

The downloaded model, settings, and your history are in `~/.local/share/fishpr/`. Uninstalling leaves them; delete the folder to remove them too.

### Build from source

You need a Rust toolchain (1.85 or later), plus **cmake** and **clang** to compile whisper.cpp, which provides the voice check, and Qt 6 for the window. On Arch:

```sh
sudo pacman -S --needed rust cmake clang qt6-base qt6-declarative kirigami qqc2-desktop-style pipewire-audio wl-clipboard libnotify
```

On Ubuntu or Debian, install Rust with [rustup](https://rustup.rs) (the distro's is too old), then:

```sh
sudo apt install build-essential cmake clang libclang-dev qt6-base-dev qt6-base-dev-tools qt6-declarative-dev qt6-declarative-dev-tools \
  qml6-module-org-kde-kirigami qml6-module-org-kde-qqc2desktopstyle qml6-module-qtquick-dialogs qml6-module-qtcore pipewire-bin wl-clipboard libnotify-bin
```

On Fedora (the distro's Rust may be too old, so use [rustup](https://rustup.rs) if `rustc --version` is below 1.85):

```sh
sudo dnf install rust cargo cmake clang clang-devel gcc-c++ qt6-qtbase-devel qt6-qtdeclarative-devel kf6-kirigami kf6-qqc2-desktop-style \
  pipewire-utils wl-clipboard libnotify
```

Then build and install:

```sh
git clone https://github.com/srafis/fishpr && cd fishpr
cargo build --release
install -Dm755 target/release/fishpr ~/.local/bin/fishpr
sed "s|^Exec=.*|Exec=$HOME/.local/bin/fishpr|" packaging/io.github.srafis.fishpr.desktop > io.github.srafis.fishpr.desktop
install -Dm644 io.github.srafis.fishpr.desktop -t ~/.local/share/applications
```

The binary is a single file, about 14 MB, that uses the system's Qt for its window. The desktop entry puts fishpr in the app menu, and the desktop portal needs it to recognize fishpr. To start fishpr on login, also copy that `io.github.srafis.fishpr.desktop` to `~/.config/autostart/`. To start it now, without logging out:

```sh
systemd-run --user --unit=app-io.github.srafis.fishpr --collect ~/.local/bin/fishpr
```

For silent pasting, also install the [uinput rule](#pasting).

On first launch fishpr downloads the voice-activity model (Silero VAD, ~1 MB) to `~/.local/share/fishpr/`.

## Usage

| Do this | Get this |
|---|---|
| Hold <kbd>Ctrl</kbd>+<kbd>Space</kbd>, speak, release | Text pasted into the focused window |
| Click the tray icon, speak, click again | Same |
| Press <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>V</kbd> | Your last transcription pasted again |
| Press <kbd>Esc</kbd> while the pill shows | Recording or transcription cancelled, or the pill hidden |
| Run `fishpr --toggle` | Starts recording, or stops and pastes, like a tray click |
| Right-click the tray icon | **Start Recording**, **Paste Last Transcription**, each with its shortcut, **Open fishpr**, and **Quit** |
| Right-click the tray icon → **Open fishpr**, or run `fishpr --window` | The window, with your history |
| Right-click the tray icon → **Quit**, or run `pkill -x fishpr` | fishpr exits and releases <kbd>Ctrl</kbd>+<kbd>Space</kbd> |

**Changing the shortcuts:** <kbd>Ctrl</kbd>+<kbd>Space</kbd>, <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>V</kbd>, and <kbd>Esc</kbd> are only the defaults, and your choices are kept across restarts. Rebind them in **System Settings → Keyboard → Shortcuts → fishpr → Push to talk (hold)**, **Paste last transcription**, and **Cancel or dismiss (while showing)**. While fishpr is running, the desktop captures the combo, so apps no longer see it. In most IDEs <kbd>Ctrl</kbd>+<kbd>Space</kbd> is "trigger suggestions", so rebind it if you miss that.

**Other desktops** (sway, Hyprland, and others) aren't supported, but may work: fishpr can't register its shortcut there, so bind the command `fishpr --toggle` to a key in the desktop's keyboard settings. It starts and stops recording like a tray click, so press once to start and again to stop.

## Pasting

fishpr pastes by pressing <kbd>Ctrl</kbd>+<kbd>V</kbd> on a virtual keyboard it creates through `/dev/uinput`. KWin doesn't let apps fake key presses any other way. It waits until you've let go of <kbd>Ctrl</kbd>, <kbd>Alt</kbd>, <kbd>Shift</kbd>, and <kbd>Meta</kbd> first, so a key still held from a shortcut doesn't turn the paste into something else. This is silent and needs no permission prompt, as long as your user can write to `/dev/uinput`.

The fishpr package installs a udev rule that grants this. KDE Connect ships the same rule. If you built from source and don't have KDE Connect, add the rule yourself:

```sh
sudo install -Dm644 packaging/60-fishpr-uinput.rules -t /etc/udev/rules.d/
```

Then log out and back in (or reboot).

Without uinput access, fishpr falls back to the desktop's RemoteDesktop portal. That works, but the desktop asks for permission once and shows a remote-control notification or indicator on every paste.

**Terminals:** most terminals paste with <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd>, so the automatic paste won't work there. The card shows the text instead: click **Copy**, then paste with <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd>.

**Older Plasma:** keeping the clipboard as it was needs KWin to support the ext-data-control protocol, which older Plasma 6 releases don't. There, fishpr leaves the text on the clipboard after pasting, as it used to.

## Troubleshooting

To see what fishpr is doing, run it in a terminal: `pkill fishpr; fishpr`. When started on login or with `systemd-run`, use `journalctl --user -u 'app-*fishpr*' -f`.

| Symptom | Likely cause |
|---|---|
| Retry keeps failing, with "HTTP 4xx/5xx" or "speech service error" in the log | The service is rate-limiting or has changed. Try again in a bit. |
| "fishpr: Ctrl+Space unavailable" | The desktop isn't KDE Plasma, or KGlobalAccel isn't running. Bind `fishpr --toggle` to a key (see [Usage](#usage)); the tray icon still works. This is only shown once. |
| "no speech detected" in the log | The voice check heard nobody. Check the input device in your desktop's sound settings. |
| The card shows up instead of pasting | Nothing that accepts text had focus, or it's a terminal (see [Pasting](#pasting)). Otherwise, no `/dev/uinput` access and the portal permission was denied. |
| No tray icon | Your panel needs the System Tray widget. |
| "fishpr is already running" | Another copy owns the shortcut and tray. Use `fishpr --toggle` to control it, or `pkill -x fishpr` first. |

## Project layout

| File | What it does |
|---|---|
| [`src/main.rs`](src/main.rs) | Tray icon, event loop (click / shortcut / quit), state and icon animation |
| [`src/transcribe.rs`](src/transcribe.rs) | Streaming client for Google's speech service and the local VAD check. This is the only file that knows about the backend. |
| [`src/recorder.rs`](src/recorder.rs) | Records the mic via `pw-record`, passing the audio on as it arrives |
| [`src/shortcut.rs`](src/shortcut.rs) | <kbd>Ctrl</kbd>+<kbd>Space</kbd>, <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>V</kbd>, and <kbd>Esc</kbd> (only while the HUD shows) via KDE's KGlobalAccel (press *and* release) |
| [`src/control.rs`](src/control.rs) | Single instance and `fishpr --toggle`, over D-Bus |
| [`src/hud.rs`](src/hud.rs) | The on-screen pill: level bars, spinner, retry button, and the card with the copy button (wlr-layer-shell, software-rendered) |
| [`src/paste.rs`](src/paste.rs) | Ctrl+V via uinput, with the portal fallback, once the modifier keys are up |
| [`src/clipboard.rs`](src/clipboard.rs) | Lends the text to the clipboard for one paste, then puts back what was there (ext-data-control) |
| [`src/desktop.rs`](src/desktop.rs) | Copying to the clipboard, and notifications |
| [`src/history.rs`](src/history.rs) | Keeps every transcription and its recording in `~/.local/share/fishpr/history/` |
| [`src/window.rs`](src/window.rs), [`src/window/`](src/window/) | The window (`fishpr --window`): a Kirigami app in QML, bridged to Rust with [CXX-Qt](https://github.com/KDAB/cxx-qt) |
| [`install.sh`](install.sh) | The one-line installer: adds the pacman or apt repo, or downloads the Fedora RPM, installs, starts |
| [`packaging/`](packaging/) | `fishpr-bin` PKGBUILD, the `.deb` builder, the RPM spec, desktop entry, uinput udev rule |
| [`site/`](site/) | The marketing page, published to GitHub Pages by [`pages.yml`](.github/workflows/pages.yml) when it changes on `main` |
| [`.github/workflows/release.yml`](.github/workflows/release.yml) | Builds, signs, and publishes a release to the pacman and apt repos and the Fedora RPM when a `v*` tag is pushed |

To switch transcription backends (for example to an official speech API), replace `Transcriber::begin` / `Session::finish` in `transcribe.rs`. Nothing else needs to change.

Run the tests with `cargo test`.

## Releasing

Releases are built by CI. The pacman repo lives in the assets of the GitHub release tagged `repo`: the package, its database, and the public signing key that `install.sh` imports. The apt repo lives the same way in the release tagged `apt`, signed with the same key. The `.deb` is built on Ubuntu 24.04, so it runs on that release's glibc and anything newer. The Fedora RPM is built in a Fedora 42 container and signed with the same key. It isn't a dnf repo, because dnf needs a `repodata/` directory and release assets can't have paths. The release tagged `rpm` holds the current RPM as `fishpr.x86_64.rpm`, plus the public key, and `install.sh` downloads those.

**One-time setup:** create a signing key without a passphrase and store it as a repo secret. Keep a backup of the key somewhere safe.

```sh
gpg --batch --passphrase '' --quick-gen-key "fishpr package signing" ed25519 sign never
gpg --armor --export-secret-keys "fishpr package signing" | gh secret set GPG_PRIVATE_KEY
```

**Each release:** bump `version` in `Cargo.toml`, commit, then tag and push:

```sh
git tag v0.2.0 && git push origin main v0.2.0
```

The tag must match `Cargo.toml`, or the workflow stops. Users get the new version on their next `pacman -Syu` or `apt upgrade`.

## Credits

- Voice activity detection: [Silero VAD](https://github.com/snakers4/silero-vad), run through [whisper.cpp](https://github.com/ggml-org/whisper.cpp) / [whisper-rs](https://codeberg.org/tazz4843/whisper-rs).
- HUD font: [Noto Sans](https://notofonts.github.io/), SIL Open Font License 1.1 ([`assets/NotoSans-OFL.txt`](assets/NotoSans-OFL.txt)).

## License

[MIT](LICENSE). The bundled Noto Sans font is under the SIL Open Font License 1.1.
