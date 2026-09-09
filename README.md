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

## Getting started

1. Connect your PC and speakers to the same local network.
2. Run `OpenAirCast.exe`.
3. Select your speakers on **Home** and choose **Apply speaker changes**.
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

Install stable Rust and the Visual Studio C++ Build Tools on Windows. Then run:

```powershell
cargo fetch --locked
cargo test --offline --locked -j 2 --target x86_64-pc-windows-msvc --workspace -- --test-threads=2
cargo build --offline --locked -j 2 --target x86_64-pc-windows-msvc --release -p homepod-cast --bin openaircast
```

The executable is written to `target/x86_64-pc-windows-msvc/release/openaircast.exe`.

## Support my work

If OpenAirCast is useful to you, you can [support my work through PayPal](https://paypal.me/ttk95). Any amount is welcome.

Bug reports are welcome too. Please include your Windows version, speaker models and the steps needed to reproduce the problem in an [issue](https://github.com/TTK95/OpenAirCast/issues).

## Credits and license

OpenAirCast builds on [windows-airplay-homepod](https://github.com/iakacer/windows-airplay-homepod) by iakacer and [airplay2-rs](https://github.com/lmcgartland/airplay2-rs) by lmcgartland.

The project uses the [GPL-2.0 license](LICENSE). Upstream copyright notices and dependency licenses are listed in [Third-Party Notices](THIRD_PARTY_NOTICES.md).
