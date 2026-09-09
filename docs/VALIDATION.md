# OpenAirCast Validation Record

The current validation status is anchored to `openaircast/main` at `5b3b4bd`.
The older GUI milestone measurements below remain useful historical evidence,
but do not supersede the current hardware confirmation or trusted baseline.

## Current hardware confirmation

On 2026-08-26, two AirPlay 2 speakers were connected at the same time and
confirmed to play simultaneously and synchronously. The production group path
completed pairing, PTP BMCA yield, shared timing and RTP fan-out. This confirms
operation, not an acoustic tolerance. No raw IP address, MAC address or receiver
clock identity is recorded in this document.

Date: 2026-08-22
Platform: Windows, stable Rust 1.98.0, MSVC target

## Historical hardware path (2026-08-22)

`openaircast.exe --selftest-group` discovered and connected to three HomePods:

- Wohnzimmer (product class: HomePod)
- Büro (product class: HomePod)
- Badezimmer (product class: HomePod)

The primary connection completed PTP BMCA and yielded to the HomePod clock.
Both secondary SETUP operations reused that identity and the primary timing
channel. The session then created three independent RTP
senders and one `AudioStreamer` with three targets.

## Real-audio exercise

While the 50-second group self-test was running, Windows played
`C:\Windows\Media\Alarm01.wav` three times through the default output. WASAPI
loopback diagnostics changed from silent frames (`rms=0.0`) to non-zero PCM:

```text
frame #3500: rms=2497.3, max_abs=5472
frame #4000: rms=2391.2, max_abs=4543
frame #5000: rms=1627.6, max_abs=4282
```

The corresponding ALAC payloads expanded from the 32-byte silent-frame form to
679, 416 and 748 bytes. Each diagnostic frame reported `targets=3`, exercising
the production one-capture, one-encode, per-receiver packet/encryption fan-out.

## Scheduler result

The run sent more than 6000 packets. At packet 6000:

```text
average jitter: 0.043 ms
maximum jitter: 5.636 ms
deadline exceedances: 6
runtime buffer underruns: 0
```

One buffer-underrun message occurred only after capture shutdown, immediately
before decoder EOF and clean process exit. The session exited with code 0.

Two Windows-specific timing defects were found and regression-tested during
hardware validation:

1. A nominal 2 ms empty-channel wait rounded to approximately 15.6 ms and
   stalled the 7.982 ms packet scheduler. Live decoding is now non-blocking by
   default.
2. A nominal 50 ms idle wait could take approximately 62.5 ms while still
   injecting only 50 ms of silence. Idle keepalive generation now follows
   measured elapsed capture time.

## Scope of this evidence

This confirms real Windows audio capture, ALAC encoding, shared PTP scheduling,
and encrypted RTP fan-out through three negotiated HomePod sessions. It does not
measure acoustic inter-speaker skew and does not cover a 30-minute run,
sleep/wake, cable/Wi-Fi interruption, automatic receiver rejoin, or repeated
start/stop leak testing.


## Control Center shell (2026-08-23, worktree `gui-control-center-implementation`)

Automated evidence for the GUI milestone, produced by the same machine:

```powershell
cargo test -p homepod-cast          # 153 passed, 0 failed, 1 ignored
cargo test --workspace --all-targets
cargo build -p homepod-cast --release
```

- All workspace suites pass with 0 failures; the three original cast tests are
  unchanged and green.
- The one ignored test is `platform::hotkey::tests::windows_message_loop_round_trip`
  (real Win32 registration); it was run explicitly once and passed.
- Release binary: `target\release\openaircast.exe`, **19,990,528 bytes**, SHA-256
  D52851EEBCEBEB04C6F5DFFE9560FBA342DC5B4EA400DC146291F9A2C22E8F51
  (baseline before the GUI milestone: 4,932,096 bytes; growth comes from the
  embedded egui/wgpu/AccessKit stack plus Inter Variable / IBM Plex Mono fonts).
- Renderer: eframe Wgpu; AccessKit enabled in debug and release.
- Packaging: `build.ps1` runs the embedded asset tests first, then copies only
  `dist\OpenAirCast.exe`; no adjacent runtime files are required.

### Manual acceptance items 1-16 (hardware, pending operator run)

The following require real HomePod hardware, a second Windows machine or VM for
Remote Desktop checks, screen readers, and human judgement. They are recorded as
**open** until executed per plan Task 15:

| # | Item | Status |
|---|---|---|
| 1 | Single window + single tray icon on Win10/11 | open |
| 2 | All Home controls operate against real receivers | open |
| 3 | Close hides without stopping; tray click restores same page | open |
| 4 | Window/tray agreement after success/failure/stop/rapid Apply | open |
| 5 | Stale backend results cannot overwrite newer state | covered by reducer/actor tests |
| 6 | Runtime hotkey replacement on hardware | open |
| 7 | Multi-second discovery does not freeze UI/Close/Stop | open |
| 8 | Accessibility Insights + Narrator keyboard pass | open |
| 9 | NVDA announcements fire once (checkbox/volume/state/failure) | open |
| 10 | Light/dark/high-contrast AA contrast + non-color distinctions | contrast math tested in code |
| 11 | DPI matrix 100-200% move without clip/jump | open |
| 12 | Reduced-motion toggle: immediate vs <=120 ms transition | transition duration unit-tested |
| 13 | wgpu under Remote Desktop | open |
| 14 | Hidden-to-tray wake after 10 minutes; Explorer restart keeps one icon | open |
| 15 | Idle CPU < 1% over 60 s visible and hidden | open |
| 16 | Quit terminates within 6 s budgets; --list/--selftest/--selftest-group still pass | quit path unit-tested; CLI re-run pending |

Operator instructions, expected outputs, and recording fields follow the plan's
Task 15 checklist. A failing manual item blocks release.

### Owner-only primary-loss hardware gate

With a running group containing at least two receivers, the owner must make the
active primary receiver deliberately unreachable. Record the deterministic
survivor rebuild, continued test tone (or the honest resulting error state), and
bounded teardown. This is a real hardware gate; it is not covered by the
translation or backend-rig tests.

## Windows Native Subproject 1 — private UI acceptance matrix (2026-08-26)

`crates/homepod-cast/src/ui/acceptance.rs` is a binary-private suite that makes
claims **across** the six destinations rather than about any one renderer. It
lives inside the binary because `crates/homepod-cast/tests` cannot see
`crate::ui` or `crate::app`.

```
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
cargo test -p homepod-cast --bin openaircast ui::acceptance::tests
                                            # 16 passed, 0 failed
cargo test -p homepod-cast --all-targets    # 764 passed, 0 failed, 2 ignored
cargo test --workspace --all-targets       # 1794 passed, 0 failed, 6 ignored
cargo check --workspace --all-targets      # clean
```

### What is machine-accepted

Each row is one assertion. "No component can" states why it had to be written
above the components rather than inside one.

| Claim | No component can, because |
|---|---|
| Every `AppEvent` variant is classified as sent by the window or excluded with a written reason. Bound to the enum twice: a wildcard-free match names and classifies a variant in the same arm, and the variant names are read out of `app/event.rs` and compared, so a variant cannot be left out of the matrix either. | A renderer knows the events it sends and nothing about the ones nobody sends. |
| Every event the window promises is produced by driving it: each control of each destination in each lifecycle state, plus the window manager's close and the live resize. | The producer and the promise live in different modules. |
| The window sends nothing it declared it never sends. | Same. |
| Every persisted `Preferences` field is changeable from the window or carries a written reason for not being. The field names are read out of `app/state.rs` and compared, because the destructuring pattern alone forces a new field to be *mentioned* and `field: _` mentions it without classifying it. | Persistence, reducer, and renderer are three separate owners. |
| The window opens at the size and maximized state it was last left at, and the one launch path still builds its viewport from the restored geometry rather than from a literal. | The value is stored by one module and consumed by another, and `run()` opens a real window, so only a read of the launch path can state the second half. |
| Every keyboard-reachable control announces a non-empty name, in German and English, in Light, Dark and Windows High Contrast, at 1120x720 and 900x600, on all six destinations, in ten lifecycle states. | The compact rail drops painted labels; only the whole window shows the combination. |
| No destination announces one name twice. | Ambiguity is a page-wide property. |
| Tab reaches every control of every destination and comes back. | The rail, the page body, and the Change Bar are three owners of one cycle. |
| No destination mixes German and English copy, in either language, in any state. | A renderer knows its own catalog, not its neighbour's sentence. |
| Every destination announces a title of its own, and the six titles are distinct. | Distinctness is a property of the set, not of a page. |
| No reachable control is inert: activating it emits a typed event or changes what the window announces; a text field must accept what is typed into it. | Whether a page's event reaches the app is a fact about the shell. |
| The declaration scan the two classification claims rest on reports a member that was just added, including one behind a doc comment and one behind an attribute. | It is the guard those two claims stand on; a scan that under-reports makes both of them vacuous. |
| No destination repeats backend text. Every failure state carries one poisoned summary through the stream state, the discovery state, and the notice at once, and no announced string may contain `os error`, `0x`, a path, `AppData`, `settings.json`, a hardware address, a service-record model identifier, or anything shaped like an IP address. | The summary enters through the snapshot, so any of the six could be the one that quotes it. |

Three defects were found by writing these and are fixed in the same change:

1. **Speakers printed a raw service-record model.** A redacted model identifier was
   rendered verbatim as the row subtitle while the Overview card had always
   resolved it to a localized device class.
2. **The Command Home offered two identical Retry buttons** in a failed
   session — the page header's lifecycle command and the notice's corrective
   action, same word, same event, nothing to tell them apart by ear.
3. **The window never learned its own size.** `AppEvent::WindowGeometryChanged`
   had a reducer arm, a persisted field, and a debounce rule written for
   geometry churn, and no producer anywhere; the viewport was additionally
   built from a hard-coded 1120x720 that ignored the restored value. Both
   halves are now wired. The window position is persisted but deliberately not
   restored — see the note in `crate::app::main_viewport`.

### What only a person can check

The matrix reads the accessibility tree. It does not render pixels, does not
run on Windows' own UI Automation stack, and cannot hear or see. Everything
below is therefore **open** and belongs to the owner:

| # | Check | Why no test can do it |
|---|---|---|
| A1 | Keyboard focus is *visible* on every control in Light, Dark and High Contrast. | The focus ring is painted; the tree only says which node has focus. A source guard asserts each custom control paints one, which is not the same as seeing it. |
| A2 | Narrator, NVDA and JAWS actually speak the names the tree carries, once each. | AccessKit's tree is the input to Windows UI Automation, not the output; duplicate or swallowed announcements only appear in a real screen reader. |
| A3 | Contrast meets AA for real, on a real display, including High Contrast themes the machine has never seen. | Contrast is computed against tokens; the OS supplies its own colours at runtime. |
| A4 | Nothing clips, overlaps, or jumps at 100/125/150/175/200 % DPI, and when the DPI changes while the window is open. | Only rectangles are asserted, and only at the two nominal sizes. |
| A5 | Reduced Motion really removes motion. | The transition duration is unit-tested; whether the eye sees a jump is not. |
| A6 | The tray icon appears once, its right-click menu opens, and its rows do what they say. | The menu model is driven and recorded by a fake surface; `tray-icon` and the Windows shell are not exercised. |
| A7 | Close hides to the tray, the tray restores the same destination, and Quit ends the process within budget. | Driven through fakes; the real window manager and the real exit are not. |
| A8 | The window really reopens at the size and maximized state it was left at, across a restart, and on a machine whose monitor set changed. | The report and the restore are each asserted; the round trip through the settings file and a fresh process is not. |
| A9 | The global shortcut works while OpenAirCast is in the background, and a reconfigured chord takes effect without a restart. | Registration is a Win32 call behind an ignored test. |
| A10 | wgpu renders under Remote Desktop and on a machine without a discrete GPU. | No renderer runs in these tests. |
| A11 | Idle CPU stays below 1 % over 60 s, visible and hidden. | Frames are stepped by the harness, not by a compositor. |
| A12 | A speaker whose hardware model OpenAirCast does not recognize shows the fallback word "Speaker"/"Lautsprecher" under a destination titled the same. Judge whether that reads as useful or as noise. | A judgement call, not an assertion. |

### Remaining validation and precision gaps

1. **The resilience backend is connected.**
   `start_device_backend` (`crates/homepod-cast/src/backend/controller.rs:291`)
   is wired through `2818f01`, `932c7fc` and `0df27bc`; recovery is visible in
   the shipping application. Rejoin, primary-device loss and network-change
   behavior still require the manual release gates.
2. **Full gPTP peer delay remains open.** The one-way offset correction is
   landed and the two-speaker result is confirmed; peer delay is the later
   precision task. Interrupted WIP `998f817` is not validation evidence.

## Baselines

Fresh package baseline at `5b3b4bd`, run today:

```text
cargo test -p homepod-cast --all-targets --no-fail-fast
929 passed, 0 failed, 2 ignored; 12/12 targets completed
```

The last known complete workspace run recorded in the handoff reported:

```text
1963 passed, 0 failed, 6 ignored
```

It was not rerun today. The smaller counts in the historical GUI sections above
are milestone records. A green process exit alone is insufficient: a trustworthy
run must also show completion summaries for every started suite.
