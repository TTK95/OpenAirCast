# Documentation

## Using OpenAirCast

- [Install and build](../README.md): download availability, portable installation and source builds.
- [Hardware check and diagnostics](HARDWARE_CHECK.md): test tone, live counters and their limits.
- [Status and remaining work](STATUS.md): verified behavior, known bugs and pending checks.

## Developing and distributing

- [Distribution](DISTRIBUTION.md): build output, packaging and publication checklist.
- [UI design](UI_DESIGN.md): detailed Windows Native layout and interaction constraints.

## Technical references

These describe the protocol and implementation behind the Windows application.

- [AirPlay 2 specification](../AIRPLAY_2_SPEC.md).
- [Apple PTP extensions](../PTP%20analysis/APPLE_PTP_TLVS.md): captured timing fields and open protocol questions.
- [Multiroom implementation](MULTIROOM_IMPLEMENTATION.md).

## Source map

- [`apps/openaircast`](../apps/openaircast): Windows application, assets and integration tests.
  Cargo package: `homepod-cast`; library: `homepod_cast`; binary: `openaircast`.
- [`app`](../apps/openaircast/src/app): state, events, effects and reducer.
- [`backend`](../apps/openaircast/src/backend): commands, session ownership and I/O.
- [`backend_bridge`](../apps/openaircast/src/backend_bridge): command translation, snapshot projection and worker lifetime.
- [`diagnostics`](../apps/openaircast/src/diagnostics): snapshots, bounded history, registry and report export.
- [`ui`](../apps/openaircast/src/ui) and [`platform`](../apps/openaircast/src/platform): rendering and Windows integration.
- [`crates`](../crates): reusable AirPlay discovery, pairing, transport, audio and timing libraries.
- [`airplay-resampler`](../crates/airplay-resampler): shared sinc resampling with TPDF dithering, used by file and live audio decoders.
- [`airplay-tui`](../crates/airplay-tui) and [`airplay-bluetooth`](../crates/airplay-bluetooth): experimental Linux terminal and BlueALSA paths; Windows tests do not validate these.
- [`build.ps1`](../build.ps1) and [`tests`](../tests): Windows packaging build and build-script checks.

## Documentation policy

Keep one current guide per subject. Public Markdown should explain installation,
usage, maintenance or substantive protocol/design behavior. Keep licenses and
third-party notices in Git.

Keep task plans, handoffs, build diaries, device-specific experiments and review
reports in the ignored `docs/local/`, `docs/plans/` or `docs/reports/` directories.
Known bugs belong in Status and release notes; release-specific hashes belong
with release assets. Superseded documents remain recoverable from Git history.
