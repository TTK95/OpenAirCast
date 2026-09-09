# Documentation

## Using OpenAirCast

- [Install and build](../README.md): download availability, portable installation and source builds.
- [Hardware check and diagnostics](HARDWARE_CHECK.md): test tone, live counters and their limits.
- [Status and remaining work](STATUS.md): verified behavior, known bugs and pending checks.

## Developing and distributing

- [Distribution](DISTRIBUTION.md): build output, packaging and publication checklist.
- [UI design](UI_DESIGN.md): detailed Windows Native layout and interaction constraints.
- [Source map and refactoring history](REFACTORING.md): implemented repository boundaries and release gates.

## Technical references

These describe protocol internals or experimental Linux paths, not additional
features promised by the Windows application.

- [AirPlay 2 specification](../AIRPLAY_2_SPEC.md) and [PTP analysis](../PTP%20analysis/PTP.md).
- [Multiroom implementation](MULTIROOM_IMPLEMENTATION.md).
- [Audio resampling](AUDIO_RESAMPLING.md) and [jitter investigation](../JITTER.md).
- [Linux Bluetooth / BlueALSA evaluation](BLUETOOTH_ALSA_EVALUATION.md).

## Documentation policy

Keep one current guide per subject. Update it instead of appending dated build
diaries or creating another handoff. Known bugs belong in Status and the actual
GitHub release notes; release-specific binary hashes belong with release assets.
Preserve licenses and substantive protocol/design references.

Historical plans, superseded release reports and obsolete PipeWire instructions
were removed from the active documentation. Their original versions remain in
Git at revision `7b02c86ff9052c8c129716634178db9296e0a68e`:

```powershell
git show 7b02c86ff9052c8c129716634178db9296e0a68e:docs/NEXT_STEPS.md
```

No source files were moved by this documentation cleanup.
