# OpenAirCast Multi-room MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:test-driven-development` task-by-task. Execute inline in this
> session because external sub-agent dispatch is not authorized.

**Goal:** Integrate the existing upstream PTP multi-target streamer into the
Windows tray app so one WASAPI capture source can stream to multiple selected
AirPlay 2 receivers.

**Architecture:** Keep the Windows capture/UI code and use one app session. A
single receiver retains the validated low-level path; two or more receivers use
`AirPlayClient::connect_group` plus `start_live_streaming_to_group`. The tray
communicates complete selected-index sets to the control thread.

**Tech Stack:** Rust 2021, Tokio, WASAPI, tray-icon, Win32, existing
`airplay-*` crates, ALAC, RTP/ChaCha20-Poly1305, gPTP/BMCA.

**Spec:** `docs/superpowers/specs/2026-08-22-openaircast-multiroom-design.md`

## Global Constraints

- Windows 10/11; normal release builds use the Windows subsystem.
- Capture exactly once per active group.
- Keep protocol code out of tray handlers.
- Use PTP for groups through the existing upstream implementation.
- Keep all queues bounded and do not add arbitrary synchronization sleeps.
- Preserve GPL-2.0 notices and both upstream attributions.
- Do not claim hardware behavior that has not been tested.

---

### Task 1: Restore a green test baseline

**Files:**
- Modify: `crates/airplay-audio/src/encoder.rs`

**Interfaces:**
- Consumes: Cargo feature `aac` from `crates/airplay-audio/Cargo.toml`.
- Produces: AAC-only tests that are compiled only when `aac` is enabled.

- [x] **Step 1: Reproduce the failing test build**

Run:

```powershell
cargo test -p airplay-audio --lib
```

Expected: compile failure resolving `AacEncoder` in the AAC test module while
the `aac` feature is disabled.

- [x] **Step 2: Apply the same feature boundary to the AAC tests**

Add `#[cfg(feature = "aac")]` to the `mod aac_encoder` test module and any
standalone AAC-only test module. Keep ALAC and shared encoder tests enabled.

- [x] **Step 3: Verify the focused and workspace tests**

```powershell
cargo test -p airplay-audio --lib
cargo test --workspace --all-targets
```

Expected: both commands exit 0; existing warnings are recorded separately.

- [x] **Step 4: Commit**

```powershell
git add crates/airplay-audio/src/encoder.rs
git commit -m "test: gate optional AAC encoder tests"
```

### Task 2: Add a testable selected-group model

**Files:**
- Create: `crates/homepod-cast/src/group_state.rs`
- Modify: `crates/homepod-cast/src/main.rs`

**Interfaces:**
- Produces: `GroupSelection::new()`, `toggle(index)`, `stop()`,
  `resume(device_count)`, `active_indices()` and `is_streaming()`.
- Consumes: only `std::collections::BTreeSet`; no Win32 or networking.

- [x] **Step 1: Write failing membership tests**

The tests construct `GroupSelection`, toggle indices 2 and 0, and assert the
literal ordered result `[0, 2]`; toggling 2 again must yield `[0]`. Separate
tests assert that `stop()` clears the active set, `resume(3)` restores the last
non-empty set, and `resume(1)` drops stale indices.

- [x] **Step 2: Run the focused tests and observe RED**

```powershell
cargo test -p homepod-cast group_state
```

Expected: compile failure because `group_state`/`GroupSelection` do not exist.

- [x] **Step 3: Implement the minimal pure model**

Use two `BTreeSet<usize>` fields: `active` and `last_non_empty`. `toggle()`
updates both when the result is non-empty; `stop()` clears only `active`;
`resume()` filters saved indices to `index < device_count` and selects index 0
only when no valid saved member exists and devices are available.

- [x] **Step 4: Verify GREEN**

```powershell
cargo test -p homepod-cast group_state
```

Expected: all membership tests pass.

### Task 3: Add one-capture multi-receiver session

**Files:**
- Modify: `crates/homepod-cast/src/cast.rs`

**Interfaces:**
- Change: `Session::start(devices: Vec<Device>, volume: f32)`.
- Internal: `SessionTransport::{Single(Connection), Group(AirPlayClient)}`.
- Preserve: `feedback`, `set_volume`, and consuming `stop` methods.

- [x] **Step 1: Add a failing validation test**

Extract a pure `validate_group(&[Device]) -> anyhow::Result<()>` boundary and
assert that an empty slice returns the literal context `"no receivers selected"`.
This catches accidental capture startup without a destination.

- [x] **Step 2: Run RED**

```powershell
cargo test -p homepod-cast cast::tests::empty_group_is_rejected
```

Expected: compile failure because `validate_group` is absent.

- [x] **Step 3: Implement the transport split**

For one device, keep `Connection::connect_auto`, NTP and the existing setup.
For two or more devices, create `AirPlayClient::with_config`, call
`connect_group`, set volume, create exactly one live sender/decoder pair and
call `start_live_streaming_to_group`. Start only one WASAPI capture thread in
both branches. On any start error, set the stop flag, join capture, and
disconnect the transport before returning the error.

- [x] **Step 4: Verify focused tests and compile**

```powershell
cargo test -p homepod-cast
cargo build -p homepod-cast
```

Expected: exit 0.

### Task 4: Convert the tray to multi-selection

**Files:**
- Modify: `crates/homepod-cast/src/tray.rs`
- Modify: `crates/homepod-cast/src/main.rs`

**Interfaces:**
- Change: `Cmd::Start(Vec<usize>)`.
- Change: `Status::Streaming(Vec<usize>)`.
- Consume: `GroupSelection` from Task 2.

- [x] **Step 1: Use the tested model in tray state**

Replace `Option<usize>` with `GroupSelection`. Each receiver menu click calls
`toggle(index)` and sends either `Start(active_indices)` or `Stop`. Checkmarks
mirror membership; the stopped item is checked only for an empty active set.

- [x] **Step 2: Update hotkey semantics**

When active, send `Stop` and call `selection.stop()`. When stopped, call
`selection.resume(device_count)` and start the restored group.

- [x] **Step 3: Update the control loop**

Map selected indices to cloned devices, stop the old session, and call
`Session::start(selected_devices, volume)`. Log the names and the fact that a
membership change renegotiates the session.

- [-] **Step 4: Verify**

```powershell
cargo fmt --all -- --check
cargo test -p homepod-cast
cargo build -p homepod-cast
```

Result: tests and build exit 0. The repository-wide format check reports the
inherited upstream formatting backlog; touched runtime files are checked
individually rather than mixing a 13,000-line mechanical rewrite into the MVP.

### Task 5: Attribution, release build and full verification

**Files:**
- Create: `THIRD_PARTY_NOTICES.md`
- Modify: `README.md`

**Interfaces:**
- Documents the exact upstream commit relationship and current MVP behavior.

- [x] **Step 1: Add attribution and user-facing MVP limitations**

Name both repositories, their GPL-2.0 relationship, the preserved Windows
changes, the reused multi-room/PTP code, the group-restart behavior, and the
physical validation still required.

- [-] **Step 2: Run fresh full verification**

```powershell
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo build -p homepod-cast --release
git diff --check
```

Result: workspace tests, release build, and `git diff --check` exit 0. Global
Rustfmt remains the inherited exception described above; upstream compiler
warnings are reported but are not test failures.

- [x] **Step 3: Inspect the artifact**

```powershell
Get-Item target\release\openaircast.exe | Select-Object FullName,Length
```

Expected: a non-empty Windows executable. Protocol-level hardware validation is
recorded in `docs/VALIDATION.md`; acoustic skew measurement remains separate.
