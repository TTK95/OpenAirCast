# Third-Party Notices

OpenAirCast/HomePod Cast is distributed under GPL-2.0. The complete license text
is in `LICENSE`.

## airplay2-rs

- Project: `lmcgartland/airplay2-rs`
- Source: https://github.com/lmcgartland/airplay2-rs
- Integrated baseline: commit `a7f019fe6246ebd9701201a8a5c31e9a15243956`
- License: GPL-2.0

This upstream provides the core AirPlay sender implementation: discovery,
HomeKit pairing, encrypted RTSP, ALAC/RTP, NTP/PTP timing, BMCA behavior,
SETPEERS, and multi-target group streaming.

## windows-airplay-homepod / HomePod Cast

- Project: `iakacer/windows-airplay-homepod`
- Source: https://github.com/iakacer/windows-airplay-homepod
- Integrated baseline: commit `cc9a2fe4b564f29f68fc4ad15cad4c1544e853aa`
- License: GPL-2.0

This fork adds the Windows application layer, including WASAPI loopback
capture, native system-tray/settings UI, volume/hotkey persistence, keepalive
behavior, release build script, and Windows portability changes in the audio
transport.

Original copyright notices and license texts are preserved. No pairing secrets,
session keys, or encryption keys are included in this notice or intended to be
written to application logs.

## GUI stack and supporting libraries (Control Center)

For every dependency below that is dual-licensed MIT/Apache-2.0, OpenAirCast
distributes under the **MIT** option. Version numbers refer to the exact,
locked versions in `Cargo.lock` of the corresponding release build.

| Component | Used version | Upstream | License option used |
|---|---|---|---|
| egui | 0.36.1 | https://github.com/emilk/egui | MIT OR Apache-2.0 → MIT |
| eframe | 0.36.1 | https://github.com/emilk/egui (eframe) | MIT OR Apache-2.0 → MIT |
| webbrowser | 1.2.4 | https://github.com/amodm/webbrowser-rs | MIT OR Apache-2.0 → MIT |
| egui-wgpu / wgpu | 0.36.1 / 30.x | https://github.com/gfx-rs/wgpu | MIT OR Apache-2.0 → MIT |
| AccessKit | 0.24.1 | https://github.com/AccessKitorg/accesskit | MIT OR Apache-2.0 → MIT |
| tray-icon | 0.24.0 | https://github.com/tauri-apps/tray-icon | MIT OR Apache-2.0 → MIT |
| arc-swap | 1.9.2 | https://github.com/vorner/arc-swap | MIT OR Apache-2.0 → MIT |
| image | 0.25.10 | https://github.com/image-rs/image | MIT OR Apache-2.0 → MIT |
| ico | 0.5.0 (dev) | https://github.com/icedland/ico | MIT |
| serde / serde_json | workspace / 1.0.149 | https://serde.rs , https://github.com/serde-rs/json | MIT OR Apache-2.0 → MIT |
| winresource | 0.1.31 (build) | https://github.com/mxre/winres + fork line | MIT |
| windows-sys | 0.59 | https://microsoft.github.io/windows-docs-rs/ | MIT OR Apache-2.0 → MIT |

The complete application remains GPL-2.0; the MIT options above are exercised
only for the linked library works as permitted by their dual licensing.

## Fonts (embedded, SIL Open Font License 1.1)

### Inter Variable

- Files: `crates/homepod-cast/assets/fonts/InterVariable.ttf`
- Source: Google Fonts tree, pinned commit
  `ec626514f79f831f1ab848a82114a0ce7e2d6372`, path `ofl/inter/Inter[opsz,wght].ttf`
- License: SIL Open Font License 1.1 (full text in
  `crates/homepod-cast/assets/licenses/Inter-OFL-1.1.txt`)
- Copyright: The Inter Project Authors (https://github.com/rsms/inter)
- The font name "Inter" and reserved font names are preserved unchanged.

### IBM Plex Mono

- Files: `crates/homepod-cast/assets/fonts/IBMPlexMono-Regular.ttf`
- Source: Google Fonts tree, pinned commit
  `ec626514f79f831f1ab848a82114a0ce7e2d6372`, path `ofl/ibmplexmono/IBMPlexMono-Regular.ttf`
- License: SIL Open Font License 1.1 (full text in
  `crates/homepod-cast/assets/licenses/IBM-Plex-Mono-OFL-1.1.txt`)
- Copyright: Copyright © 2017 IBM Corp.
- The font name "IBM Plex" and reserved font names are preserved unchanged.
