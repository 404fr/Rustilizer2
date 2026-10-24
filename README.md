# Rustilizer2

A lightweight, click-through rainbow audio equalizer bar that sits directly on top of your Windows taskbar.

## Features

- **Sits On Your Taskbar**: Rests directly on top of the Windows taskbar border (`Shell_TrayWnd`) with 110 logarithmically-spaced frequency bars (`30 Hz` – `16 kHz`).
- **WASAPI Loopback Audio Capture**: Captures desktop system audio output in real time with zero configuration via `cpal` and `rustfft`.
- **Rainbow Spectrum Modes**: Smooth HSV rainbow colors with 3 selectable modes (*Flowing Wave*, *Static Spectrum*, *Center Mirror*) and optional peak-hold caps.
- **Compact & Customizable Layout**:
  - **Position**: Left of Screen *(default)*, Center of Screen, or Right of Screen.
  - **Size Presets**: Small (`28% × 16px`), Mini (`18% × 12px`), Medium (`45% × 20px`), and Full-Width (`100% × 16px`).
- **Click-Through Transparent Overlay**: Uses a per-pixel alpha Win32 layered window (`WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST`) so mouse clicks pass right through and it never steals focus or appears in `Alt+Tab`.
- **Disable on Fullscreen**: Automatically hides the equalizer when a fullscreen game, video, or presentation is active (toggleable via the system tray menu or `--no-fullscreen-hide`).

## Quick Start

### Prerequisites

- Windows 10 or Windows 11
- [Rust toolchain](https://rustup.rs/) (stable)

### Build & Run

```powershell
cargo run --release
```

In release mode, the binary runs as a pure Windows GUI app (`windows_subsystem = "windows"`) without opening a console window.

### Command-Line Options

| Flag | Description |
| --- | --- |
| `--no-fullscreen-hide` | Start with automatic fullscreen hiding disabled |

## System Tray Controls

Right-click the **Rustilizer2** icon in your Windows system tray to customize on the fly:

- **Disable on Fullscreen**: Toggle automatic hiding when fullscreen apps/games are active.
- **Show Peak Caps**: Toggle the floating peak-hold dots above the bars.
- **Rainbow Mode**: Cycle between *Flowing Wave*, *Static Spectrum*, and *Center Mirror*.
- **Bar Size**: Cycle between *Small (`28% × 16px`)*, *Mini (`18% × 12px`)*, *Medium (`45% × 20px`)*, and *Full-Width (`100% × 16px`)*.
- **Position**: Cycle between *Left of Screen*, *Center of Screen*, and *Right of Screen*.
- **Exit Rustilizer2**: Cleanly close the overlay and remove the tray icon.

## License

Licensed under the [MIT License](LICENSE).
