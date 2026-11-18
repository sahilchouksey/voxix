<p align="center">
  <img src="core/icons/logo.png" alt="voxix logo" width="120" />
</p>
<h1 align="center">voxix</h1>

<p align="center">
  <em>Local speech-to-text. Press a shortcut, speak, and your words appear — nothing leaves your machine.</em>
</p>

voxix is a cross-platform desktop speech-to-text application. Press a keyboard shortcut, speak, and your words appear in whatever text field you are focused on — transcribed entirely on your own computer. No audio leaves the machine, and nothing is sent to a cloud service.

## How it works

1. Press a global shortcut and speak.
2. Audio is recorded locally and filtered through voice activity detection.
3. A local transcription model produces the text.
4. The text is typed or pasted into the focused application.

Models are chosen in the app's model selector. voxix uses local inference engines (`transcribe.cpp` for Whisper-family GGUF models and ONNX-based models such as Parakeet, Moonshine, and SenseVoice). Model files are downloaded once from the voxix model catalog and are then used fully offline.

Coverage at a glance:

- **Shortcuts** — push-to-talk or toggle-style bindings, multiple concurrent bindings, app-specific shortcuts.
- **Streaming** — streaming-capable models transcribe while you are still speaking.
- **Translation** — a one-click translate-to-English mode on supporting models.
- **Dictionary** — custom words appended to the model's vocabulary to improve recognition of names and domain vocabulary.
- **Post-processing** — optionally pipe transcripts through a large language model (your own API keys) to fix spelling, punctuation, and formatting.
- **History** — searchable transcription history with save/retry/delete actions and configurable retention.
- **Overlay** — a floating recording/transcribing pill (platform support varies).

## Installing

There are no published binary releases yet. Install from source:

```bash
git clone https://github.com/sahilchouksey/voxix.git
cd voxix
bun install
bun run tauri build
```

Platform-specific build prerequisites are described in [BUILD.md](BUILD.md).

## Using voxix from the command line

The `voxix` binary accepts flags to control a running instance and to run headlessly:

| Flag                     | Description                                                     |
| ------------------------ | --------------------------------------------------------------- |
| `--start-hidden`         | Launch without showing the main window                          |
| `--no-tray`              | Disable the system tray icon                                    |
| `--toggle-transcription` | Toggle recording on/off on a running instance                   |
| `--toggle-post-process`  | Toggle transcription with post-processing on a running instance |
| `--cancel`               | Cancel the current operation on a running instance              |
| `--debug`                | Enable verbose (trace) logging                                  |
| `--transcribe-file FILE` | Transcribe a WAV file (16 kHz mono) headlessly and exit         |
| `--list-models`          | List available models and exit                                  |
| `--list-devices`         | List transcription compute devices and exit                     |
| `--json`                 | Emit machine-readable JSON for the headless commands            |
| `--help`                 | Show all flags                                                  |

On Unix systems a running instance can also be toggled with signals:

| Signal    | Action                                                                                                                         |
| --------- | ------------------------------------------------------------------------------------------------------------------------------ |
| `SIGUSR2` | Toggle plain recording                                                                                                         |
| `SIGUSR1` | Toggle recording with post-processing (macOS only — on Linux, `SIGUSR1` is used internally by WebKitGTK and must not be bound) |

This makes integrations with window managers, hotkey daemons, and scripts straightforward:

```bash
voxix --toggle-transcription
pkill -USR2 -x -n voxix
```

## Debug mode

Enable verbose logging with `voxix --debug`, or launch the app with the debug-mode setting. Logs are written to the app data directory shown on the About screen of the settings window.

## Privacy

- **Audio never leaves your computer.** Recording, voice activity detection, and transcription all run on-device.
- **No telemetry.** voxix phones home for nothing. Update checking exists but is disabled by default and currently not wired to a feed.
- **Models are downloaded directly** from the voxix model catalog over HTTPS; downloads are verified with SHA-256 hashes.
- **Post-processing and transcription helpers connect only to endpoints you configure yourself.** If you never configure an LLM provider, no network request is made during normal use.

## Troubleshooting

### Text does not appear after recording (Linux)

Typing into other applications depends on display-server support:

- On X11 and XWayland targets voxix types via enigo.
- On Wayland (wlroots-style compositors) choose a direct typing tool such as `wtype`, `dotool`, `ydotool`, or `kwtype` in Settings → Typing tool.
- On GNOME Wayland and KDE Wayland the compositor lacks the virtual-keyboard protocol, so use an external typing tool rather than the default Auto selection.

### Linux startup crashes or instability

- Missing shared libraries are the most common cause; see [BUILD.md](BUILD.md) for the list of runtime packages.
- If the overlay behaves unexpectedly with the GTK layer shell, set `VOXIX_NO_GTK_LAYER_SHELL=1` to disable that mode.

### macOS

- Grant **Accessibility** permission so voxix can type text, and **Microphone** permission for recording. If scheduled recordings silently stop, check System Settings → Privacy & Security.

### Windows

- Use the Windows paste method setting if the direct paste does not land in the focused application. Vulkan runtime is used for GPU acceleration of Whisper-family models.


## Contributing

Community contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md), and [CONTRIBUTING_TRANSLATIONS.md](CONTRIBUTING_TRANSLATIONS.md) for translations.

## License

voxix is MIT licensed. See [LICENSE](LICENSE).

## Primary references (Handy & Hex)

While building voxix, the primary reference implementations were **Handy** and **Hex**. At the time of voxix development, Hex was Mac-only — I built this for myself initially, locally, to use it on Linux; now I have ported it for macOS, Linux, and Windows (though it has not yet been fully tested).
