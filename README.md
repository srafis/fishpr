<p align="center">
  <img src="assets/icon-idle.png" width="160" alt="fishpr">
</p>

<h1 align="center">fishpr</h1>

<p align="center">
  Push-to-talk dictation for KDE Plasma on Linux.<br>
  Hold <kbd>Ctrl</kbd>+<kbd>Space</kbd>, speak, release, and the text is typed where your cursor is.
</p>

---

## What it does

- **Hold to talk.** Hold <kbd>Ctrl</kbd>+<kbd>Space</kbd> anywhere. A small "Recording…" pill appears at the bottom of the screen. Let go and the transcription lands in the focused window about half a second later.
- **Or click the tray icon.** Click once to start, click again to stop. Tray and shortcut share one state, so you can start with one and stop with the other.
- **Pastes for you.** The text goes on the clipboard and is pasted into the focused app with <kbd>Ctrl</kbd>+<kbd>V</kbd>. If pasting fails, it's still on your clipboard.
- **Quiet when it works.** A notification appears only when something goes wrong.
- **Ignores silence.** A local voice check runs before any result is requested, so an accidental press with nobody speaking does nothing, instead of pasting a made-up "Thank you."

Tray icon states:

| Idle | Recording | Transcribing |
|:---:|:---:|:---:|
| <img src="assets/icon-idle.png" width="64"> | <img src="assets/icon-active.png" width="64"> | <img src="assets/icon-loading.gif" width="64"> |

## How it works

Audio is streamed to Google's speech service while you're still talking. It's the same service the dictation button on gemini.google.com uses. Because streaming happens during the recording, only the end-of-audio marker is left when you release the key, so the text comes back fast whatever the clip length.

```
pw-record ──PCM──▶ fishpr ──100 ms chunks──▶ Google speech (WebChannel)
    │                  │                              │
  mic               on release:                 final text
                    local VAD ── no speech? ──▶ drop session
                        │
                      speech ──▶ end-of-audio ──▶ clipboard + Ctrl+V
```

> [!WARNING]
> **This uses an unofficial, undocumented endpoint.** It uses the Gemini web app's public API key and identifies itself as gemini.google.com. Google can change or block it at any time without notice. Your audio is sent to Google, so don't dictate anything you wouldn't type into Gemini.

## Requirements

- **KDE Plasma 6 on Wayland.** fishpr uses KDE's global-shortcut service and KWin's overlay (layer-shell) protocol. Clicking the tray icon also works on other desktops with a StatusNotifier tray.
- **PipeWire** for `pw-record`. This is the default on Arch.
- **wl-clipboard** for `wl-copy`, or `xclip` on X11.
- **libnotify** for `notify-send`.
- **Access to `/dev/uinput`** for silent pasting. If you have KDE Connect installed you already have it. Otherwise see [Pasting](#pasting).

To build: a Rust toolchain, plus **cmake** and **clang**. These compile whisper.cpp, which provides the voice check.

```sh
sudo pacman -S --needed rust cmake clang pipewire wl-clipboard libnotify
```

## Install

```sh
git clone <this repo> fishpr && cd fishpr
cargo build --release
```

The binary is `target/release/fishpr` (about 14 MB, self-contained). Copy it anywhere, for example:

```sh
install -Dm755 target/release/fishpr ~/.local/bin/fishpr
```

### API key

fishpr needs the Gemini web app's API key. It isn't included in the source. To get it:

1. Open [gemini.google.com](https://gemini.google.com), open DevTools → **Network**, and click the microphone button in the prompt box.
2. Find a request to `speechs3proto2-pa.googleapis.com/…/streaming/channel` and copy its `x-goog-api-key` request header.
3. Save it:

```sh
mkdir -p ~/.config/fishpr
printf '%s' 'PASTE_KEY_HERE' > ~/.config/fishpr/gemini-api-key
chmod 600 ~/.config/fishpr/gemini-api-key
```

Or set `FISHPR_GEMINI_API_KEY` in fishpr's environment.

> [!TIP]
> If you exported a `.har` file from DevTools, you can pull the key out of it directly:
> ```sh
> python3 -c "import json,sys;h=json.load(open(sys.argv[1]));print(next(x['value'] for e in h['log']['entries'] if 'speechs3proto2' in e['request']['url'] for x in e['request']['headers'] if x['name'].lower()=='x-goog-api-key'))" gemini.google.com.har > ~/.config/fishpr/gemini-api-key
> ```
> Delete the `.har` afterwards, because it contains your Google session cookies.

### Start on login

```sh
mkdir -p ~/.config/autostart
cat > ~/.config/autostart/fishpr.desktop <<EOF
[Desktop Entry]
Type=Application
Name=fishpr
Comment=Push-to-talk dictation
Exec=$HOME/.local/bin/fishpr
Icon=audio-input-microphone
X-KDE-autostart-phase=2
EOF
```

Or run it right away without logging out:

```sh
systemd-run --user --unit=app-fishpr --collect ~/.local/bin/fishpr
```

On first launch fishpr downloads the voice-activity model (Silero VAD, ~1 MB) to `~/.local/share/fishpr/`.

## Usage

| Do this | Get this |
|---|---|
| Hold <kbd>Ctrl</kbd>+<kbd>Space</kbd>, speak, release | Text pasted into the focused window |
| Click the tray icon, speak, click again | Same |
| Right-click the tray icon → **Quit** | fishpr exits and releases <kbd>Ctrl</kbd>+<kbd>Space</kbd> |

**Changing the shortcut:** <kbd>Ctrl</kbd>+<kbd>Space</kbd> is only the default. Rebind it in **System Settings → Keyboard → Shortcuts → fishpr → Push to talk (hold)**, and your choice is kept across restarts. While fishpr is running, KDE captures the combo, so apps no longer see it. In most IDEs <kbd>Ctrl</kbd>+<kbd>Space</kbd> is "trigger suggestions", so rebind it if you miss that.

## Pasting

fishpr pastes by pressing <kbd>Ctrl</kbd>+<kbd>V</kbd> on a virtual keyboard it creates through `/dev/uinput`. KWin doesn't let apps fake key presses any other way. This is silent and needs no permission prompt, as long as your user can write to `/dev/uinput`.

KDE Connect ships a udev rule that grants this. Without KDE Connect, add the same rule yourself:

```sh
echo 'KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uinput"' \
  | sudo tee /etc/udev/rules.d/60-fishpr-uinput.rules
```

Then log out and back in (or reboot).

Without uinput access, fishpr falls back to the desktop's RemoteDesktop portal. That works, but KDE asks for permission once and shows a "Remote control session started" notification on every paste.

**Terminals:** most terminals paste with <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd>, so the automatic paste won't work there. The text is still on your clipboard.

## Troubleshooting

To see what fishpr is doing, run it in a terminal: `pkill fishpr; fishpr`. When started with `systemd-run`, use `journalctl --user -u app-fishpr -f`.

| Symptom | Likely cause |
|---|---|
| "fishpr couldn't start: no Gemini API key" | Set up the [API key](#api-key). |
| "Transcription failed: … handshake failed: HTTP 403" | The key is wrong or has changed. Grab a fresh one from gemini.google.com. |
| "fishpr: Ctrl+Space unavailable" | Not running on KDE Plasma, or KWin's shortcut service isn't reachable. The tray icon still works. |
| "no speech detected" in the log | The voice check heard nobody. Check the mic in System Settings → Audio. |
| Pastes do nothing | Terminal (see above), or no `/dev/uinput` access and the portal permission was denied. |
| No tray icon | Your panel needs the System Tray widget. |

## Project layout

| File | What it does |
|---|---|
| [`src/main.rs`](src/main.rs) | Tray icon, event loop (click / shortcut / quit), state and icon animation |
| [`src/transcribe.rs`](src/transcribe.rs) | Streaming speech client (WebChannel + protobuf) and the local VAD check. This is the only file that knows about the backend. |
| [`src/recorder.rs`](src/recorder.rs) | Streams mic PCM from `pw-record` |
| [`src/shortcut.rs`](src/shortcut.rs) | <kbd>Ctrl</kbd>+<kbd>Space</kbd> via KGlobalAccel (press *and* release) |
| [`src/hud.rs`](src/hud.rs) | The "Recording…" overlay (wlr-layer-shell, software-rendered) |
| [`src/paste.rs`](src/paste.rs) | Ctrl+V via uinput, with the portal fallback |
| [`src/desktop.rs`](src/desktop.rs) | Clipboard and notifications |

To switch transcription backends (for example to the official OpenAI API), replace `Transcriber::begin` / `Session::finish` in `transcribe.rs`. Nothing else needs to change.

Run the tests with `cargo test`.

## Credits

- Voice activity detection: [Silero VAD](https://github.com/snakers4/silero-vad), run through [whisper.cpp](https://github.com/ggml-org/whisper.cpp) / [whisper-rs](https://codeberg.org/tazz4843/whisper-rs).
- HUD font: [Noto Sans](https://notofonts.github.io/), SIL Open Font License 1.1 ([`assets/NotoSans-OFL.txt`](assets/NotoSans-OFL.txt)).
