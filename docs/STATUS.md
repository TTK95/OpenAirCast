# Status and remaining work

Updated: 2026-09-10. Source implementation: `3e3b036`. The source-layout
refactoring is implemented and source verification passed; final review,
optimized build, packaging and publication remain pending.
This is a status summary, not a claim of production certification.

## Available

- Windows loopback streaming to one or several AirPlay speakers; master and individual levels.
- Saved groups, source selection, tray, global shortcut, German/English and theme settings.
- A generated test tone and live diagnostics for captured input, sender buffering and local transmission.

The owner confirmed playback on three speakers, separate levels and the generated
test tone during development. These confirmations are not a new test of every
subsequent binary and do not measure acoustic synchronization.

## Verification

The full workspace run after the source refactoring finished successfully:
**2,162 passed, 0 failed, 17 ignored**. The earlier recorded baseline was
2,167 passed, 0 failed and 17 ignored. Task 5 intentionally removed two tests
with their test-only helpers. Removing the integration test's private `cast.rs`
inclusion also stopped three live `cast` unit tests from running a duplicate
second time; those tests remain in the binary suite.

```powershell
cargo test --workspace --locked --target x86_64-pc-windows-msvc --target-dir <shared-target> -j2 -- --test-threads=2
```

This run did not enable ignored hardware/manual tests, launch the app or start
speaker playback. Scoped Clippy also passed with the 7 existing `openaircast`
warnings, and the build-script regression suite passed. Existing dependency
and compiler warnings remain. Automated tests are not a clean Windows
installation check or a fresh physical listening test.

The previously documented optimized local EXE predates the source-layout
refactoring. A new optimized build from the final reviewed revision is still
required. Local outputs are not published downloads. See
[Distribution](DISTRIBUTION.md) and the [installation guide](../README.md) for
the distinction.

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
- [x] Implement and independently verify source-layout Tasks 1–4.
- [x] Audit and remove the two proven test-only helpers in a separate cleanup commit.
- [ ] Complete final source-layout review and verification; see the
  [source map and history](REFACTORING.md).
- [ ] Reproduce and fix Suspend/Cancel with a failing regression test first;
  run the remaining manual Windows/hardware matrix only with authorization.
- [ ] Package the chosen revision, publish the first GitHub release with its
  known bug, verify downloads and update the README. A regular release label
  must not imply that the outstanding checks passed.
