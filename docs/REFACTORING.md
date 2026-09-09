# OpenAirCast source refactoring proposal

**Status:** implementation authorized; tasks below are checked off only after verification.

**Goal:** make the Windows product immediately identifiable and reduce the
number of responsibilities held in very large Rust modules without changing behavior.

**Architecture:** separate product location from reusable libraries, then split
modules behind their existing interfaces. Keep the single-owner backend actor,
pure shell reducer and snapshot-driven UI.

**Stack and constraints:** Rust 2021, app Rust 1.95+, Cargo workspace, Windows
MSVC, egui/eframe. No new framework, dependency upgrade or protocol change.
Design constraints: [UI design](UI_DESIGN.md). Known behavior: [Status](STATUS.md).

## Findings

The workspace has a useful protocol-crate separation. OpenAirCast now lives in
`apps/openaircast`, making the Windows product distinct from the reusable
libraries under `crates`.

| Current location | Responsibility |
|---|---|
| `apps/openaircast` | Windows product: binary `openaircast`, package `homepod-cast`, library `homepod_cast` |
| `crates/airplay-tui` | Separate terminal application, including Linux-oriented paths |
| Other `crates/airplay-*` | Protocol, crypto, discovery, audio, timing and reusable client libraries |
| `apps/openaircast/src/app` | Shell state, events, effects and reducer |
| `apps/openaircast/src/backend` | Commands, state owner and I/O supervisors |

The largest files include `backend_bridge.rs` (~7,976 lines), `app/reducer.rs`
(~4,622), `ui/presentation.rs` (~4,022), `backend/controller.rs` (~3,958) and
`diagnostics.rs` (~3,900). These totals **include tests**. `ui/pages/mod.rs`
is ~3,892 lines but mostly tests; splitting it is not evidence that its renderer
needs an architectural rewrite.

## Options and recommendation

1. **Recommended: distinguish apps from libraries, then split incrementally.**
   Clear product entry point; localized manifest/path updates; behavior remains stable.
2. Keep all packages under `crates` and add a source map only. Lowest risk,
   but the product remains harder to identify in the file tree.
3. Merge libraries into one app or rename every package/module at once. Reject:
   large review surface, unnecessary API churn and greater regression risk.

`apps/` is a project organization choice, not a Cargo requirement. Keep Rust's
standard `src`, `tests`, `examples` and `benches` names. Cargo documents these
conventions in [Package Layout](https://doc.rust-lang.org/cargo/guide/project-layout.html);
[Workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html) allow
explicit member paths and a shared lockfile/build output.

## Target layout

```text
OpenAirCast/
├── Cargo.toml / Cargo.lock       workspace, shared dependency resolution
├── apps/
│   ├── openaircast/              Windows product (first migration)
│   │   ├── Cargo.toml           package remains homepod-cast initially
│   │   ├── assets/              existing fonts/icons and licenses
│   │   ├── src/
│   │   │   ├── main.rs          CLI dispatch and app startup
│   │   │   ├── lib.rs           existing public contract
│   │   │   ├── app/             state, reducer, effects
│   │   │   ├── backend/         single-owner actor and I/O supervisors
│   │   │   ├── backend_bridge/  command translation and snapshot projection
│   │   │   ├── diagnostics/     registry, snapshots and safe export
│   │   │   ├── ui/              pages, components, presentation, localization
│   │   │   └── platform/        Windows integration
│   │   └── tests/              public-contract integration tests
│   └── airplay-tui/             terminal product (separate later migration)
├── crates/airplay-*/             reusable libraries
├── docs/                        indexed current documentation
├── tests/                       repository/build-script checks
├── build.ps1                    supported Windows build entry point
└── target/ and dist/            ignored compiler output and packaging
```

The tree emphasizes changed boundaries, not every retained file. `cast.rs`,
`device_service.rs`, `preferences.rs`, `tray.rs` and `setup_diagnostic.rs` stay
in place until their callers and ownership have been reviewed. Legacy labels
or `allow(dead_code)` do not justify deleting live CLI/transport paths.

## Migration tasks

Execute one task per reviewable commit. Do not combine moves with bug fixes.
Paths below are repository-relative. Stop on failed verification; never weaken
a behavioral assertion to make a move pass.

### Task 1: Make the Windows product visible

Files: move the Windows product to `apps/openaircast/`; update root
`Cargo.toml`, the moved `Cargo.toml` and every actual tracked reference to the
old folder. Inspect `build.ps1`, `tests/build-script.tests.ps1`, CI and docs;
change only paths that depend on the move.

- [ ] Record package/target/dependency metadata before moving:
  `cargo metadata --no-deps --format-version 1 --locked`.
- [ ] Move the directory with Git. Change the workspace member to
  `apps/openaircast`; rewrite its `../airplay-*` dependencies to
  `../../crates/airplay-*`. Retain package `homepod-cast`, library
  `homepod_cast`, binary `openaircast`, version, features and dependency versions.
- [ ] Search for tracked references to the previous application path; update live consumers,
  including source-string tests, asset loading and relative `include_str!`
  paths. Preserve root `LICENSE` and `THIRD_PARTY_NOTICES.md`.
- [ ] Compare Cargo metadata: same package names, targets, features and
  dependencies; only application manifest/source paths differ. Run checks below.
- [ ] Commit the move only. Rollback is a revert of this commit, not a hard reset.

**Acceptance:** all 12 workspace members remain present; existing build commands,
asset embedding, examples and public imports still resolve. No settings migration.

### Task 2: Separate large test bodies without widening APIs

Files under `apps/openaircast/src`: `ui/pages/mod.rs`, `ui/presentation.rs`,
`app/reducer.rs` and adjacent private test modules.

- [ ] List current tests with `cargo test -p homepod-cast --bin openaircast -- --list`.
- [ ] Move self-contained inline `#[cfg(test)] mod tests { ... }` bodies to
  adjacent `tests.rs` modules, one parent at a time. Use `#[cfg(test)] mod tests;`
  and preserve the existing test-module nesting and `use super::*` boundaries.
- [ ] Keep binary-private UI tests inside the binary. Do not make internals
  public merely to move them into Cargo integration tests.
- [ ] Compare names/counts before and after, and run the affected suite. Source
  inspection guards must inspect the new files, not silently lose coverage.
- [ ] Commit only after the existing acceptance matrix passes.

**Acceptance:** identical test discovery/behavior, no new public API, production
files easier to read. No arbitrary line-count quota or mechanical splitting
of functions that share an invariant.

### Task 3: Split the backend bridge behind the existing module name

Files: replace `apps/openaircast/src/backend_bridge.rs` with
`backend_bridge/mod.rs`, `commands.rs`, `projection.rs`, `runtime.rs` and
private `tests.rs` as responsibilities are extracted.

- [ ] Keep the module name `backend_bridge`; keep `main.rs` consumers compiling
  through narrowly scoped re-exports in `mod.rs`.
- [ ] Extract pure command/effect translation first; retain its existing tests.
- [ ] Extract snapshot projection second, preserving generation/revision guards,
  unknown/stale state handling and group-request correlation.
- [ ] Extract worker/channel lifetime management last. Preserve bounded queues,
  shutdown budgets and the single owner of mutable backend state.
- [ ] Run `backend_bridge::` and `ui::acceptance::` suites, then package tests;
  review and commit each extraction separately.

**Acceptance:** no I/O in rendering or the pure reducer; no new shared mutable
owner; late responses cannot resurrect stopped sessions or confirm newer requests.

### Task 4: Split diagnostics, then review actor responsibilities

Files: `apps/openaircast/src/diagnostics.rs` to `diagnostics/mod.rs` with
private `registry.rs`, `snapshot.rs`, `export.rs`, `tests.rs`; inspect
`backend/controller.rs`, `backend/capture.rs` and `backend/session.rs` separately.

- [ ] Preserve every existing public diagnostics export through `mod.rs`.
- [ ] Extract safe report formatting independently from mutable registry state.
- [ ] Keep session/registration ownership, sample freshness and bounded history
  with their tests. No raw names, addresses, paths or errors in copied reports.
- [ ] Only extract actor/capture helpers where ownership remains explicit;
  do not create a generic `utils` module or relocate state across threads.
- [ ] Run diagnostics, capture and controller suites plus the package suite
  after each extraction. Commit separately from Task 3.

**Acceptance:** unknown is not zero; local send counters are not remote reception;
test-tone samples do not masquerade as measured Windows input. No timing changes.

### Task 5: Finish the repository boundary and release handoff

Files: optionally move `crates/airplay-tui/` to `apps/airplay-tui/` with its
manifest, workspace and script consumers; update `docs/README.md` and this map.

- [ ] Validate the TUI's supported platform/dependency requirements before its
  own move. If no matching validation environment is available, leave it in
  `crates` and record that explicit exception; do not claim Windows tests cover Linux.
- [ ] Audit `device_service.rs` and legacy bridge/cast seams using caller searches,
  feature/cfg inspection and tests. Remove only proven unused code in a separate
  commit; preserve `--list`, `--selftest`, `--selftest-group`, `--diagnose-group`.
- [ ] Run full workspace tests, scoped lint and a new optimized Windows build.
- [ ] Review the final diff for unintended behavior/dependency/lockfile changes.
- [ ] Package the exact chosen revision using [Distribution](DISTRIBUTION.md).
  Publish only after verification; retain the known Suspend/Cancel note.

## Verification commands

Run from the repository root, one Cargo process at a time. Use two jobs/test
threads to limit load. Do not run ignored tests or launch playback automatically.
For task-specific runs, use the relevant filter before the final `--`.

```powershell
cargo metadata --no-deps --format-version 1 --locked
cargo check --workspace --all-targets --locked --target x86_64-pc-windows-msvc -j2
cargo test -p homepod-cast --locked --target x86_64-pc-windows-msvc -j2 -- --test-threads=2
cargo test --workspace --locked --target x86_64-pc-windows-msvc -j2 -- --test-threads=2
cargo clippy -p homepod-cast --lib --bin openaircast --no-deps --locked --target x86_64-pc-windows-msvc -j2
./tests/build-script.tests.ps1
./build.ps1
git diff --check
```

Check exit status after each command; stop on failure. Compare existing lint
warnings rather than claiming an already-warning baseline is clean. Format only
changed Rust modules; avoid unrelated formatting churn. UI-only smoke checks and
physical speaker checks are separate, permission-bound steps. These commands
are planned verification, not evidence that the proposed structure already exists.
