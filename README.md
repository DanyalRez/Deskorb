<div align="center">

<img src="assets/orbite/deskorb-orbite-512.png" width="112" alt="Deskorb logo">

# Deskorb

**Your wireless mouse battery and your Claude Code usage, right in the taskbar.**

A tiny Windows tray app with a Liquid Glass panel: see how much charge your Pulsar mouse has left and how much of your Claude Code 5-hour window you've burned, without opening anything.

![Windows 10/11](https://img.shields.io/badge/Windows-10%20%2F%2011-2b2d31?style=flat-square)
![Native Rust](https://img.shields.io/badge/Rust-native-2b2d31?style=flat-square)
![0% CPU at rest](https://img.shields.io/badge/CPU%20at%20rest-0%25-2b2d31?style=flat-square)
![~13 MB RAM](https://img.shields.io/badge/RAM-~13%20MB-2b2d31?style=flat-square)

<img src="docs/media/deskorb.gif" width="360" alt="Deskorb in use: open the panel, open the settings, switch theme, icon and language">

▶ [Watch the 24 s demo video](docs/media/deskorb.mp4)

</div>

---

## What it does

<table>
<tr>
<td width="50%"><img src="docs/media/panel.png" alt="The panel above the taskbar"></td>
<td width="50%"><img src="docs/media/settings.png" alt="The settings"></td>
</tr>
<tr>
<td><b>One click away.</b> Click the tray icon: a glass panel rises above the taskbar with your mouse battery, whether it's wired, wireless, charging or asleep, and your token window: used, limit, what's left and when it resets.</td>
<td><b>Make it yours.</b> Theme, tray icon, panel density, token limit, refresh rate, language, low-battery alert and launch at startup, all live, all animated.</td>
</tr>
<tr>
<td><img src="docs/media/themes.png" alt="The three themes"></td>
<td><img src="docs/media/settings-fr.png" alt="Settings in French"></td>
</tr>
<tr>
<td><b>Three themes.</b> Liquid Glass (the background refracts through the panel), Dark and Light.</td>
<td><b>English or French.</b> Follows your Windows language on first launch, switch anytime.</td>
</tr>
</table>

### A tray icon that is a widget

Pin the icon next to the clock (drag it out of the hidden icons) and you can read your status without clicking. Pick its style in the settings:

| <img src="assets/orbite/deskorb-orbite-512.png" width="64"> | <img src="assets/jauges/deskorb-jauges-512.png" width="64"> | **42** |
| :---: | :---: | :---: |
| **Orbit**: one ring that fills with your battery (or your tokens when no mouse is found) | **Gauges**: outer ring = battery, inner ring = tokens | **Percent**: just the number |

## Supported hardware and data

- **Mice**: Pulsar wireless mice on the 8K "dx" dongle (`3710:5406`, e.g. X2 CrazyLight / TenZ Edition), wired or wireless. Other dongles speaking the same protocol (Kysona, ATK, VXE family) may work but are untested.
- **Tokens**: read locally from Claude Code's transcripts (`~/.claude/projects`). No API key, no network. Anthropic doesn't publish plan limits as token counts, so the limit is either **Auto** (your biggest past 5-hour window) or a value you set with the slider.

## Use

| Action | Effect |
| --- | --- |
| Click the tray icon | opens the panel |
| Click the gear in the panel | opens the settings |
| Right-click the tray icon | Settings, Quit |
| Click elsewhere or press Esc | closes |

### Settings

- **Theme**: Liquid Glass, Dark, Light
- **Tray icon**: Orbit, Gauges, Percent
- **Panel**: Detailed or Compact
- **Token limit**: Auto, or 5 M to 1,000 M
- **Refresh every**: 5 s, 15 s, 1 min
- **Language**: English, Français
- **Low battery alert**: a Windows notification under 20 %
- **Launch at startup**

Saved in `%APPDATA%\Deskorb\settings.txt`.

## Under the hood

- **Native Rust**, a single dependency (`windows-sys`), a ~500 KB executable.
- **Liquid glass engine from [DESKTOUCH](https://github.com/DanyalRez/DESK-TOUCH)**: the background is captured and blurred, a lens refracts the edges, colours split slightly in the corners, a light rim follows the shape. Spring animations paced to your screen's refresh rate; **nothing runs at rest**.
- **Mouse battery over HID, no driver**: 17-byte vendor reports on the dongle's `FF02` collection, the same protocol as the Linux kernel's `hid-pulsar` driver. When the mouse sleeps it stops answering, so Deskorb keeps the last value and says *asleep*.
- **Tokens**: Claude Code's JSONL transcripts are read incrementally (only new bytes), deduplicated per message and grouped into 5-hour windows the way Claude Code counts them.

## Build from source

```bash
cargo build --release
```

```bash
target/release/deskorb.exe
```

Needs Rust with the `x86_64-pc-windows-gnu` target and MinGW (`windres` embeds the icons).

Two diagnostic flags print to the console:

```bash
cargo run -- --probe
```

```bash
cargo run -- --tokens
```

`--probe` lists HID interfaces and reads the mouse battery; `--tokens` prints the current window.

## Credits

- Battery protocol: the Linux [`hid-pulsar`](https://github.com/torvalds/linux/blob/master/drivers/hid/hid-pulsar.c) driver; device IDs cross-checked with [pulsar-mouse-linux](https://github.com/packerlschupfer/pulsar-mouse-linux).
- Glass recipe adapted from [AndroidLiquidGlass](https://github.com/Kyant0/AndroidLiquidGlass) (Kyant, Apache-2.0), via DESKTOUCH.
- Logos: `assets/orbite/` and `assets/jauges/` (SVG sources, PNG, `.ico` from 16 to 256 px).

Not affiliated with Pulsar Gaming Gears or Anthropic.
