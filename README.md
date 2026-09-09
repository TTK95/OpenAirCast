# OpenAirCast

Stream Windows audio to HomePods and other AirPlay 2 speakers.

I maintain OpenAirCast as [TTK95](https://github.com/TTK95). It captures audio from a Windows playback device and sends it to one or more speakers. The app runs as a portable executable, with a control window and a system tray icon.

## What it does

- Finds AirPlay 2 speakers on your local network.
- Streams to a single speaker or a group.
- Provides master volume and separate levels for each speaker.
- Saves speaker groups and their volume settings.
- Lets you select the Windows playback device to capture.
- Keeps playing when you close the window to the tray.
- Offers a global shortcut, German and English text, and light and dark themes.
- Includes a guided speaker test and a diagnostics report you can copy.

## Download and install (Windows x64)

OpenAirCast is portable: there is currently no MSI/EXE installer and no `winget`
package. You do not need Rust or Git to run a packaged version.

**Availability checked on 2026-09-09:** this repository has no public GitHub
Release yet. Local builds and prepared ZIP files are not published downloads.
Until a release is uploaded, use [Build from source](#build-from-source).

When a release is available:

1. Open [GitHub Releases](https://github.com/TTK95/OpenAirCast/releases).
2. Under **Assets**, download the Windows x64 portable ZIP, not GitHub's
   automatically generated **Source code** ZIP.
3. Extract the complete ZIP to a folder you own, such as
   `%LOCALAPPDATA%\Programs\OpenAirCast`. This is a suggested location, not a
   folder that already exists on your PC.
4. Run `OpenAirCast.exe` from the extracted folder. Administrator mode is not
   normally required. An unsigned development build is not a signed stable release.

For updates, quit the app from its tray menu before replacing its extracted
files. User settings are stored separately in `%APPDATA%\OpenAirCast`.

## First use

1. Connect your PC and speakers to the same local network.
2. Run `OpenAirCast.exe`.
3. Select your speakers on **Overview** and apply the selection changes.
4. Start streaming and play audio on the selected Windows playback device.
5. Adjust the master volume or individual speaker levels.

The default shortcut is **Ctrl+Alt+H**. Closing the window keeps the app in the tray. Choose **Quit** from the tray menu to exit.

If speakers appear but cannot connect, check that Windows Firewall allows OpenAirCast on your private network.

For a short speaker test, open **Audio → Open hardware check**. The [hardware-check guide](docs/HARDWARE_CHECK.md) explains the test and the information included in its report.

## Status and limitations

OpenAirCast is still in development. I have tested playback with three speakers and separate volume levels. Long listening sessions, sleep/wake recovery and clean Windows installations still need more testing.

- Changing the speakers in a running group restarts the group.
- AirPlay introduces a delay. Do not expect real-time monitoring or lip-sync with every video player.
- Precise synchronization between speakers still needs measurement on hardware.
- Suspending Windows during connection and cancelling just after wake can leave the app at **Stopping**. If this happens, quit from the tray and restart the app.
- Launch at Windows startup is not implemented.

The [development notes](docs/NEXT_STEPS.md) track remaining work. The [validation notes](docs/VALIDATION.md) record hardware tests.

## Build from source

Cloning this repository downloads source code, **not an installed application**.
Paths below are relative to the folder created by `git clone`; they are not
paths on the maintainer's PC.

Prerequisites on Windows x64:

- Git for Windows.
- Rust with the MSVC toolchain (the app requires Rust 1.95 or newer).
- Visual Studio C++ Build Tools with **Desktop development with C++**, including
  the MSVC compiler/linker and a Windows SDK. Open a new terminal after installation.

In PowerShell, from a directory where you want to keep the source:

```powershell
git clone --branch openaircast/main https://github.com/TTK95/OpenAirCast.git
cd OpenAirCast
rustup target add x86_64-pc-windows-msvc
.\build.ps1
.\dist\OpenAirCast.exe
```

`build.ps1` runs the embedded-asset test, builds the optimized Windows x64 app
with the lockfile and two compiler jobs, then copies it to `dist\OpenAirCast.exe`.
The first build needs internet access to download dependencies and can take a
while. It does not run the complete test suite or publish anything to GitHub.

If your PowerShell policy blocks scripts, use the equivalent Cargo commands
without changing the policy:

```powershell
cargo test --locked -j 2 --target x86_64-pc-windows-msvc --target-dir target -p homepod-cast --bin openaircast ui::theme::tests::embedded_assets_are_present_and_icon_has_required_sizes -- --exact --test-threads=2
cargo build --locked -j 2 --target x86_64-pc-windows-msvc --target-dir target --release -p homepod-cast --bin openaircast
.\target\x86_64-pc-windows-msvc\release\openaircast.exe
```

Run each command only if the preceding command succeeds. The manual commands
leave the executable in `target`; they do not refresh `dist`.

For the full application test suite (no physical playback tests):

```powershell
cargo test --locked -j 2 --target x86_64-pc-windows-msvc --target-dir target -p homepod-cast -- --test-threads=2
```

## Where the final version belongs

| Location | Purpose |
|---|---|
| GitHub **Releases → Assets** | Public, versioned portable ZIP and matching source; publication is a separate step. |
| Your extracted download folder | The copy you actually run; no repository is needed. |
| `dist\OpenAirCast.exe` | Local optimized output after a successful `build.ps1`; not a download link or proof of publication. |
| `target\x86_64-pc-windows-msvc\release\openaircast.exe` | Cargo's optimized build output. |
| `target\x86_64-pc-windows-msvc\debug\openaircast.exe` | Development/testing build, not the distributable release. |

`target/` and `dist/` are intentionally excluded from Git. Existing local ZIPs
are not updated by `build.ps1` and may contain older code. See the
[distribution checklist](docs/DISTRIBUTION.md) for packaging and publication.

## Support my work

If OpenAirCast is useful to you, you can [support my work through PayPal](https://paypal.me/ttk95). Any amount is welcome.

Bug reports are welcome too. Please include your Windows version, speaker models and the steps needed to reproduce the problem in an [issue](https://github.com/TTK95/OpenAirCast/issues).

## Credits and license

OpenAirCast builds on [windows-airplay-homepod](https://github.com/iakacer/windows-airplay-homepod) by iakacer and [airplay2-rs](https://github.com/lmcgartland/airplay2-rs) by lmcgartland.

The project uses the [GPL-2.0 license](LICENSE). Upstream copyright notices and dependency licenses are listed in [Third-Party Notices](THIRD_PARTY_NOTICES.md).
