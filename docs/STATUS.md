# Status and remaining work

Updated: 2026-09-10. [Release v0.1.0](https://github.com/TTK95/OpenAirCast/releases/tag/v0.1.0)
uses source revision `dafc765c3234f7a4b4177a2cbf9494d45c4cec9a`.
This is a status summary, not a claim of production certification.

## Available

- Windows loopback streaming to one or several AirPlay speakers; master and individual levels.
- Saved groups, source selection, tray, global shortcut, German/English and theme settings.
- A generated test tone and live diagnostics for captured input, sender buffering and local transmission.

The owner confirmed playback on three speakers, separate levels and the generated
test tone during development. These confirmations are not a new test of every
subsequent binary and do not measure acoustic synchronization.

## Verification

The recorded Windows workspace run for the release finished with
**2,162 passed, 0 failed, 17 ignored**:

```powershell
cargo test --workspace --locked --target x86_64-pc-windows-msvc --target-dir target -j2 -- --test-threads=2
```

This run did not enable ignored hardware/manual tests, launch the app or start
speaker playback. Scoped Clippy also passed with the 7 existing `openaircast`
warnings, and the build-script regression suite passed. Existing dependency
and compiler warnings remain. Automated tests are not a clean Windows
installation check or a fresh physical listening test.

Build details and hashes ship with the release assets. See
[Distribution](DISTRIBUTION.md) and the
[installation guide](../README.md).

## Known limitations

1. **Suspend/Cancel bug:** suspending during connection and cancelling shortly
   after wake can leave the shell at **Stopping**. Quit from the tray and restart.
2. **Timing:** acoustic inter-speaker skew, full peer-delay support, prolonged
   stability and real network/primary-device-loss recovery still need validation.
   Changing active group membership restarts the group.
3. **Latency profiles:** only Normal is available. Low/Stable require defined
   parameters and a developer hardware matrix; a successful hearing check does
   not unlock them. Diagnostics do not measure room sound, end-to-end latency
   or confirmed remote packet reception.
4. **Windows integration:** the native DPI/accessibility, screen-reader, tray,
   shortcut, RDP and idle-load matrix is not fully accepted on current binaries.
5. **Distribution:** unsigned portable application; no installer, automatic
   updater or launch-at-login feature. v0.1.0 is published with these limitations.

## Next work, in order

- Reproduce and fix Suspend/Cancel with a regression test.
- Complete the remaining Windows integration and clean-installation checks.
- Measure physical speaker synchronization and verify long sessions and recovery.
