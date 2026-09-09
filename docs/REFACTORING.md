# OpenAirCast source map and refactoring history

**Status:** source refactoring implemented and source verification passed;
final review, release build, packaging and publication remain separate gates.

The refactoring made the Windows product visible in the repository and split
two large internal modules without changing package names, public interfaces,
protocol behavior or the single-owner backend model.

## Current repository map

```text
OpenAirCast/
├── Cargo.toml / Cargo.lock
├── apps/
│   └── openaircast/
│       ├── Cargo.toml
│       ├── assets/
│       ├── src/
│       │   ├── main.rs
│       │   ├── lib.rs
│       │   ├── app/
│       │   ├── backend/
│       │   ├── backend_bridge/
│       │   ├── diagnostics/
│       │   ├── ui/
│       │   └── platform/
│       └── tests/
├── crates/
│   ├── airplay-tui/
│   └── airplay-*/
├── docs/
├── tests/
└── build.ps1
```

`apps/openaircast` is the Windows product. Its Cargo package remains
`homepod-cast`, its library remains `homepod_cast`, and its binary remains
`openaircast`. The other `airplay-*` packages remain reusable protocol,
discovery, audio, timing and client crates.

The terminal application intentionally remains at `crates/airplay-tui`. It
depends on Linux-only Bluetooth/BlueALSA and ALSA paths, and this refactoring
had no validated Linux environment. Windows test results therefore do not
claim to validate or justify moving that application.

## Implemented module boundaries

The shell state, events, effects and reducer remain under `src/app`; backend
commands, state ownership and I/O supervisors remain under `src/backend`.
Private test bodies were moved beside their parent modules without widening
production visibility.

The former `backend_bridge.rs` is now:

- `backend_bridge/mod.rs` — stable module boundary and narrow re-exports
- `backend_bridge/commands.rs` — effect and command translation
- `backend_bridge/projection.rs` — backend-to-shell snapshot projection
- `backend_bridge/runtime.rs` — worker, channel and lifetime management
- `backend_bridge/tests.rs` — private regression tests

The former `diagnostics.rs` is now:

- `diagnostics/mod.rs` — preserved diagnostics exports
- `diagnostics/snapshot.rs` — diagnostic state and snapshot types
- `diagnostics/ring.rs` — bounded history storage
- `diagnostics/registry.rs` — mutable registration and session ownership
- `diagnostics/export.rs` — privacy-safe report formatting
- `diagnostics/tests.rs` — private regression tests

`cast.rs`, `device_service.rs`, `preferences.rs`, `tray.rs` and
`setup_diagnostic.rs` remain in place. The live rollback and migration seams
`legacy_device_seam`, `effect_to_command` and `legacy_volume_file` remain, as
do `cast::discover`, `cast::Session` and `cast::DEFAULT_VOLUME`.

## Completed history

- [x] Task 1 moved the Windows application from `crates/homepod-cast` to
  `apps/openaircast` while retaining all 12 workspace members and Cargo target names.
- [x] Task 2 extracted large private test bodies without creating public APIs.
- [x] Task 3 split `backend_bridge` by responsibility behind its existing module name.
- [x] Task 4 split diagnostics while preserving exports, bounded history and ownership.
- [x] Task 5 audited legacy/cast seams and removed only the two test-only helpers
  `device_service::newest_volume` and `cast::{LaunchRoute, route_args}` with their tests.

Tasks 1–4 each passed their scoped acceptance suites before the next task.
Task 5 leaves `main.rs` command dispatch for `--list`, `--selftest`,
`--selftest-group` and `--diagnose-group` unchanged. Removing the integration
test's private inclusion of all of `cast.rs` also stops three live `cast` unit
tests from running a duplicate second time; they remain in the binary suite.

## Verification and release handoff

- [x] Full-workspace Windows tests: 2,162 passed, 0 failed, 17 ignored.
- [x] Scoped Clippy completed with the 7 existing `openaircast` warnings.
- [x] Build-script regression suite passed without compiling Rust or launching the app.
- [ ] Complete final review and merge the source commits.
- [ ] Build the optimized Windows binary from the reviewed revision.
- [ ] Package that exact revision using [Distribution](DISTRIBUTION.md).
- [ ] Publish only after package verification; retain the known Suspend/Cancel note.

Automated Windows tests do not replace UI smoke checks, a clean-machine check,
physical-speaker playback, acoustic synchronization measurements or Linux TUI
validation. Those remain separate, permission-bound work.
