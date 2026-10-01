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
- **Pastes for you.** The text goes on the clipboard and is pasted into the focused app with <kbd>Ctrl</kbd>+<kbd>V</kbd>. If pasting fails, it's still on your clipboard.
- **Retry when it fails.** If transcription fails, the pill stays for three seconds with a retry button, and a click sends the same recording again. Nothing you said is lost to a network hiccup.
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
                     └──▶ clipboard + Ctrl+V
```

> [!WARNING]
> **This uses an unofficial, undocumented endpoint.** Google can change or block it at any time without notice. Your audio is sent to Google as you speak, including on an accidental press where nobody talks, so don't dictate anything you wouldn't say to Gemini.

## Requirements

- **KDE Plasma 6, on Wayland.** GNOME isn't supported.
- **Arch Linux, Ubuntu 24.04 or later, or Debian 13 or later, on x86_64** for the one-line installer. On anything else, [build from source](#build-from-source).

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/srafis/fishpr/main/install.sh | sh
```

The installer adds fishpr's signed package repo and installs fishpr from it:

- **Arch:** adds the `[fishpr]` repo to `/etc/pacman.conf` and installs `fishpr-bin`. It uses `pacman -Syu`, so it also upgrades the rest of your system.
- **Ubuntu and Debian:** adds `/etc/apt/sources.list.d/fishpr.sources`, with the signing key in `/etc/apt/keyrings/fishpr.asc` (trusted for fishpr's repo only), and installs `fishpr`.

The package brings in the dependencies (PipeWire, wl-clipboard, libnotify), installs the [uinput rule](#pasting) for silent pasting, and makes fishpr start on login. The installer then starts fishpr.

If you installed fishpr by hand before, delete `~/.local/bin/fishpr` and `~/.config/autostart/fishpr.desktop`. The old autostart entry would otherwise override the packaged one.

### Updating

New versions arrive with your normal system upgrade (`sudo pacman -Syu` or `yay` on Arch, `sudo apt upgrade` on Ubuntu and Debian). The running copy keeps its old version until you restart it: run `pkill -x fishpr`, then launch fishpr from the app menu. Or run the install command again, which updates and restarts it in one go.

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

The downloaded model and settings are in `~/.local/share/fishpr/`.

### Build from source

You need a Rust toolchain (1.85 or later), plus **cmake** and **clang** to compile whisper.cpp, which provides the voice check. On Arch:

```sh
sudo pacman -S --needed rust cmake clang pipewire-audio wl-clipboard libnotify
```

On Ubuntu or Debian, install Rust with [rustup](https://rustup.rs) (the distro's is too old), then:

```sh
sudo apt install build-essential cmake clang libclang-dev pipewire-bin wl-clipboard libnotify-bin
```

Then build and install:

```sh
git clone https://github.com/srafis/fishpr && cd fishpr
cargo build --release
install -Dm755 target/release/fishpr ~/.local/bin/fishpr
sed "s|^Exec=.*|Exec=$HOME/.local/bin/fishpr|" packaging/io.github.srafis.fishpr.desktop > io.github.srafis.fishpr.desktop
install -Dm644 io.github.srafis.fishpr.desktop -t ~/.local/share/applications
```

The binary is self-contained, about 14 MB. The desktop entry puts fishpr in the app menu, and the desktop portal needs it to recognize fishpr. To start fishpr on login, also copy that `io.github.srafis.fishpr.desktop` to `~/.config/autostart/`. To start it now, without logging out:

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
| Run `fishpr --toggle` | Starts recording, or stops and pastes, like a tray click |
| Right-click the tray icon → **Quit**, or run `pkill -x fishpr` | fishpr exits and releases <kbd>Ctrl</kbd>+<kbd>Space</kbd> |

**Changing the shortcut:** <kbd>Ctrl</kbd>+<kbd>Space</kbd> is only the default, and your choice is kept across restarts. Rebind it in **System Settings → Keyboard → Shortcuts → fishpr → Push to talk (hold)**. While fishpr is running, the desktop captures the combo, so apps no longer see it. In most IDEs <kbd>Ctrl</kbd>+<kbd>Space</kbd> is "trigger suggestions", so rebind it if you miss that.

**Other desktops** (sway, Hyprland, and others) aren't supported, but may work: fishpr can't register its shortcut there, so bind the command `fishpr --toggle` to a key in the desktop's keyboard settings. It starts and stops recording like a tray click, so press once to start and again to stop.

## Pasting

fishpr pastes by pressing <kbd>Ctrl</kbd>+<kbd>V</kbd> on a virtual keyboard it creates through `/dev/uinput`. KWin doesn't let apps fake key presses any other way. This is silent and needs no permission prompt, as long as your user can write to `/dev/uinput`.

The fishpr package installs a udev rule that grants this. KDE Connect ships the same rule. If you built from source and don't have KDE Connect, add the rule yourself:

```sh
sudo install -Dm644 packaging/60-fishpr-uinput.rules -t /etc/udev/rules.d/
```

Then log out and back in (or reboot).

Without uinput access, fishpr falls back to the desktop's RemoteDesktop portal. That works, but the desktop asks for permission once and shows a remote-control notification or indicator on every paste.

**Terminals:** most terminals paste with <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd>, so the automatic paste won't work there. The text is still on your clipboard.

## Troubleshooting

To see what fishpr is doing, run it in a terminal: `pkill fishpr; fishpr`. When started on login or with `systemd-run`, use `journalctl --user -u 'app-*fishpr*' -f`.

| Symptom | Likely cause |
|---|---|
| Retry keeps failing, with "HTTP 4xx/5xx" or "speech service error" in the log | The service is rate-limiting or has changed. Try again in a bit. |
| "fishpr: Ctrl+Space unavailable" | The desktop isn't KDE Plasma, or KGlobalAccel isn't running. Bind `fishpr --toggle` to a key (see [Usage](#usage)); the tray icon still works. This is only shown once. |
| "no speech detected" in the log | The voice check heard nobody. Check the input device in your desktop's sound settings. |
| Pastes do nothing | Terminal (see above), or no `/dev/uinput` access and the portal permission was denied. |
| No tray icon | Your panel needs the System Tray widget. |
| "fishpr is already running" | Another copy owns the shortcut and tray. Use `fishpr --toggle` to control it, or `pkill -x fishpr` first. |

## Project layout

| File | What it does |
|---|---|
| [`src/main.rs`](src/main.rs) | Tray icon, event loop (click / shortcut / quit), state and icon animation |
| [`src/transcribe.rs`](src/transcribe.rs) | Streaming client for Google's speech service and the local VAD check. This is the only file that knows about the backend. |
| [`src/recorder.rs`](src/recorder.rs) | Records the mic via `pw-record`, passing the audio on as it arrives |
| [`src/shortcut.rs`](src/shortcut.rs) | <kbd>Ctrl</kbd>+<kbd>Space</kbd> via KDE's KGlobalAccel (press *and* release) |
| [`src/control.rs`](src/control.rs) | Single instance and `fishpr --toggle`, over D-Bus |
| [`src/hud.rs`](src/hud.rs) | The on-screen pill: level bars, spinner, retry button (wlr-layer-shell, software-rendered) |
| [`src/paste.rs`](src/paste.rs) | Ctrl+V via uinput, with the portal fallback |
| [`src/desktop.rs`](src/desktop.rs) | Clipboard and notifications |
| [`install.sh`](install.sh) | The one-line installer: adds the pacman or apt repo, installs, starts |
| [`packaging/`](packaging/) | `fishpr-bin` PKGBUILD, the `.deb` builder, desktop entry, uinput udev rule |
| [`.github/workflows/release.yml`](.github/workflows/release.yml) | Builds, signs, and publishes a release to both repos when a `v*` tag is pushed |

To switch transcription backends (for example to an official speech API), replace `Transcriber::begin` / `Session::finish` in `transcribe.rs`. Nothing else needs to change.

Run the tests with `cargo test`.

## Releasing

Releases are built by CI. The pacman repo lives in the assets of the GitHub release tagged `repo`: the package, its database, and the public signing key that `install.sh` imports. The apt repo lives the same way in the release tagged `apt`, signed with the same key. The `.deb` is built on Ubuntu 24.04, so it runs on that release's glibc and anything newer.

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
