# Status and remaining work

Updated: 2026-09-09. Source implementation: `07872d6863f0f671f332cb969b4fd4285dd08965`.
This is a status summary, not a claim of production certification.

## Available

- Windows loopback streaming to one or several AirPlay speakers; master and individual levels.
- Saved groups, source selection, tray, global shortcut, German/English and theme settings.
- A generated test tone and live diagnostics for captured input, sender buffering and local transmission.

The owner confirmed playback on three speakers, separate levels and the generated
test tone during development. These confirmations are not a new test of every
subsequent binary and do not measure acoustic synchronization.

## Verification

The full workspace run on checkout `7b02c86ff9052c8c129716634178db9296e0a68e`
finished successfully: **2,167 passed, 0 failed, 17 ignored**.

```powershell
cargo test --workspace --offline --locked --target x86_64-pc-windows-msvc -j2 -- --test-threads=2
```

`--offline` assumes dependencies are already cached. This run did not enable
ignored hardware/manual tests, launch the app or start speaker playback.
Existing dependency/compiler warnings remain. Automated tests are not a clean
Windows installation check or a fresh physical listening test.

The optimized local EXE was built successfully from the application source above.
Local outputs are not published downloads. See [Distribution](DISTRIBUTION.md)
and the [installation guide](../README.md) for the distinction.

## Known limitations

1. **Suspend/Cancel bug:** suspending during connection and cancelling shortly
   after wake can leave the shell at **Stopping**. Quit from the tray and restart.
   The owner accepted releasing with this issue disclosed; it is not fixed.
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
   updater or launch-at-login feature. Publication is still pending.

## Next work, in order

- [x] Consolidate documentation; preserve design and technical references.
- [ ] Review the [source-layout refactoring proposal](REFACTORING.md).
- [ ] Execute approved structural changes separately from behavioral fixes.
- [ ] Reproduce and fix Suspend/Cancel with a failing regression test first;
  run the remaining manual Windows/hardware matrix only with authorization.
- [ ] Package the chosen revision, publish the first GitHub release with its
  known bug, verify downloads and update the README. A regular release label
  must not imply that the outstanding checks passed.
