# OpenAirCast — Current Execution Handoff

> **Owner override, 2026-09-09:** Commit, merge and push are now explicitly
> authorized despite the known Suspend/Cancel bug. Treat it as an accepted
> integration risk, not a fixed defect. The earlier no-merge/no-push hold below
> is historical. Release preparation and its remaining verification limits are
> described in [release notes](RELEASE_NOTES_2026-09-09.md). The first follow-up
> code task remains the exact Suspend-during-activation / Cancel-before-resume
> regression and fix. No new playback or standby test is authorized.

> **Current handoff, 2026-09-09:** The approved five-point implementation is
> integrated into `openaircast/main` in the main checkout. See the
> [release check](RELEASE_CHECK_2026-09-09.md) for fresh automated evidence,
> native-check limits and the remaining manual gates. This is not a 1.0 release
> approval. The [complete product plan](superpowers/specs/2026-09-08-openaircast-complete-product-design.md)
> retains the broader product and release requirements.

> **For agentic workers:** REQUIRED SUB-SKILL: use `superpowers:subagent-driven-development` task-by-task, `superpowers:test-driven-development` for every behavior change, and `superpowers:verification-before-completion` before every completion claim. Use independent read-only review after each task. Do not run two implementation agents against the same files.

## Immediate next action

1. **Next code fix: accepted Suspend/Cancel defect.** Reproduce
   **Suspend during activation → Cancel before the two-second resume settle**,
   publish the prepared generation's Stopped acknowledgement after bounded
   cleanup, and obtain focused review and fresh verification. The owner accepts
   this defect for the current integration; it is not fixed. All three earlier
   volume/editor findings were addressed in `047dfc6`.
2. Work in the main checkout on `openaircast/main`, not the retired GUI worktree.
   The current optimized executable is `dist/OpenAirCast.exe`; portable and
   corresponding-source ZIP files are beside it. Use
   [artifact checksums](RELEASE_ARTIFACTS_2026-09-09.md) and
   [integration notes](INTEGRATION_2026-09-09.md) to identify the clean version.
   Native visual/navigation/accessibility acceptance is still unperformed:
   capture failed with `SetIsBorderRequired`/`0x80004002`. No fresh app launch
   or playback is implied by packaging or integration.
3. Obtain owner authorization for the physical checklist in the release check:
   30-minute multiroom playback and measured skew, independent levels through
   restart/rejoin, disconnect/rejoin, standby/resume and real tray exit/relaunch.
   Do not run competing playback sessions or disrupt the network implicitly.
4. Continue the broader 1.0 gates: measured synchronization and long-run
   stability, normal-user Windows 10/11 and DPI/NVDA acceptance, packaging,
   notices/license audit, signing and security. Full gPTP peer delay remains
   conditional precision work, release-blocking if the current synchronization
   gates fail. WIP `998f817` remains unreviewed; do not adopt it blindly.

| Five-point scope | Implemented evidence | Remaining limit |
|---|---|---|
| 1. Preserve the verified baseline (Task 1) | `113984b`: multiroom and individual-level baseline preserved | Historical owner confirmation of three speakers and independent levels; no new long-run listening test |
| 2. Safe levels before audio (Task 2) | `4ce6b7d`, `c4b8ebe`, `08d83c8`, `a5136e6`: startup levels and later updates ordered before production transport audio | Physical startup/rejoin listening remains to verify |
| 3. Compact everyday UI and real endpoints (Tasks 3–4) | `1ac5c31`, `951aa87`, `e9b7dde`: shared draft, compact controls, bounded off-thread scan and honest refresh/failure status | Native DPI/accessibility and physical endpoint switching/default changes remain separate |
| 4. Saved groups backend and UI (Tasks 5–6) | `21af72b`, `4373e6b`, `38d3bc8`, `9eebc0e`: durable templates, exact results, local editor and explicit apply/start/delete | Native restart workflow and physical group playback remain to verify |
| 5. Release checks and handoff (Task 7) | `b639d30`, `e720ff5`, `3595057`, `047dfc6`; [current test/build/review record](RELEASE_CHECK_2026-09-09.md), including the production audio gate, restored master and offline reselection | Original three findings closed; Suspend/Cancel remains OPEN and explicitly accepted by the owner for integration; no full 1.0 acceptance claim |

The owner's 2026-09-09 three-speaker and individual-level confirmation is
historical evidence, not a new run of this build. Current software checks and
manual evidence are listed separately in the release check.

Fresh verification of source `047dfc6070cf42d8404aca779dd17c57e5af6358`:
**2,104 passed, 0 failed, 15 ignored** across 44 workspace targets including
doctests; scoped Clippy and normal debug build exited 0 with existing warnings.
Normal EXE SHA-256:
`55201E6BEB941BBE07F0957C5A23744DFE2D8DC7C0BD7142B069E0B5B71B12A6`.
It was not launched. These green checks do not close the Important Suspend/Cancel
issue or make the software merge-ready. See the release check for exact commands,
artifact path/time/size, warning counts and the old `b639d30` native-only evidence.

## Historical investigation and execution records

The material below preserves earlier diagnoses and plans. Its old counts,
next steps, app state and authorization text are not the current handoff above.

## Two speakers were silent: fixed, and what the fix costs

**Resolved on 2026-08-26.** The owner confirms both receivers now play
simultaneously and in sync. What follows is the diagnosis, because the cause
was not where anyone would look and the fix is deliberately an approximation.

### The cause

A HomePod's PTP clock is arbitrary-epoch: measured at 1 886 724 s, counting
from its own boot about 21.8 days earlier. The sender adopted that clock's
*identity* while keeping its own *timebase* -- the log showed `master`
bit-identical to the sender's Unix wall clock -- so receivers were handed
presentation times roughly **56 years** in the future and dropped every packet.

One defect, both speakers: the secondary mirrors the primary's offset through a
watch channel rather than measuring its own. That is also why a single receiver
always worked -- the single path runs NTP with the sender as its own reference,
where an offset of zero is true by construction. **PTP had never carried audio
on this setup.**

### Why the offset was never measured

Measured in one session: **156 `Delay_Req` sent, 0 answers**, while the same
device sent 154 `Follow_Up`, 78 `Announce` and 79 `Signaling`. AirPlay 2 speaks
**gPTP (IEEE 802.1AS)**, whose delay measurement is the *peer* mechanism --
`Pdelay_Req`/`Pdelay_Resp`/`Pdelay_Resp_Follow_Up`. `PtpMessageType` implements
none of them. The module calls itself gPTP throughout while asking its central
question in the other dialect, and a gPTP grandmaster ignores it.

### The fix, and what it approximates

`crates/airplay-timing/src/ptp.rs`, landed in `9ea7793`:

1. `Delay_Resp` is accepted on the **general** socket, where clause 13.8 of
   IEEE 1588-2008 puts it and where `Announce` and `Follow_Up` already arrive.
   Correct, and inert until something answers.
2. `one_way_offset(t1, t2)` publishes `t2 - t1` when nothing answers, with the
   omitted path delay declared in `error_ns` rather than hidden.

The estimate is wrong by one link traversal -- tens to a few hundred
microseconds -- against an audio budget measured in milliseconds. In practice
that is inaudible; the owner reports clean synchronisation.

**Peer delay is therefore a precision improvement, not a blocker.** Implement it
when the estimate proves insufficient, and let it *replace* the estimate rather
than sit beside it.

### Two things this cost, worth not repeating

**Absent log output is not a finding.** Three conclusions were drawn from
missing lines that could never have appeared: once the mutation was not applied
at all (the script had aborted), once the line was at `trace` while `debug` was
enabled, once the test tone had already stopped. Verify that output *could*
appear before reading its absence.

**A silent source is indistinguishable from the bug.** With nothing playing on
Windows the capture yields 32-byte silence packets and the receivers are
correctly silent. Play audio during any listening test, and confirm packet
lengths (~210-250 bytes for real ALAC) before asking anyone what they hear.

### Reproducing a timing investigation

`RUST_LOG="warn,airplay_timing=trace,airplay_client=info,airplay_audio=debug"`.
`Delay_Req` and `Delay_Resp` are `trace!` lines.

### Still open on the group path

* `timingPeerList` names only the sender (`crates/airplay-rtsp/src/session.rs`,
  `build_setup_phase1`): `peer_list = vec![peer_info]`. A member is never told
  about the others. It plays anyway now, so this is not the blocker it looked
  like -- but a domain whose members do not know each other is not a domain.
* `run_ptp_group_master_flow` and `Connection::setup_as_ptp_master` have **no
  callers**. Making the sender grandmaster (priority1=246) was tried and
  reverted: the receivers keep announcing and never accept the claim.



The following paragraph is superseded by the 2026-08-26 hardware confirmation
above; it is retained as historical investigation context. Raw device
addresses have been removed.

## Status is read from commits, not from checkboxes

**The checkboxes in every plan document are unreliable.** Not one is ticked,
although the majority of the work is done. Read `git log` instead. The table
below was reconstructed from the commit history on 2026-08-25 and supersedes
the counts that used to stand here — those listed Task 10 as open although
`39808ce` had implemented it.

| Resilience task | State | Landed in |
|---|---|---|
| 1-14 | done | see `git log master..HEAD` |
| 15 Backend controller | done | `350801b` |
| 15b Production transport | done | `9fe7c56` |
| 16 steps 1-4 Health, isolation, retry | done | `63cccaf` |
| 16 steps 5-7 Rejoin, primary loss, storms | **DONE** | `a8d07d8` |
| 17 Network, suspend/resume, mute, latency | **DONE** | `ae488e4`, `6980563`, `4ee603`, `fdf3170` |
| 18 Shell integration | superseded by Windows Native SP2 | - |
| 19 Diagnostics, cleanup, failure storms | **open** | - |
| 20 Release and hardware gates | **open**, partly owner-only | - |
| — Connect the backend to the app | **done** | bridge `2818f01`, switched on `932c7fc`, recovery made visible `9c463b6`, diagnostics page `ed574fd` |

Diagnostics/calibration: 8 done, the calibration domain landed with `350801b`;
task 10 (per-target presentation clocks) with `fe21f1e`; task 11 (support-safe
export) with `e75667b`, except its step 8 effect/reducer wiring. Tasks 9 and
12-15 remain open; 12 is superseded by Windows Native SP3.

Windows Native GUI: SP1 Task 1 done (`a6d4387`, `c0cda0d`), Tasks 2, 5 and 6
with `99370bd`. Task 3 and everything that waited on it landed since; the
Settings destination completed with `2c6e5e0`, merged as `b1327cc`. Task 10 —
the private acceptance matrix — is implemented on branch `feat/acceptance`
and **not committed**; see the section below.

### Windows Native SP1 Task 10 (landed as `436eed4`, merged `b578cfa`)

`crates/homepod-cast/src/ui/acceptance.rs` (new, ~1330 lines) is a
binary-private matrix that asserts across the whole surface rather than beside
it. What it covers, and what only a person can check, is recorded in
[`VALIDATION.md`](VALIDATION.md).

Writing it found three defects; all three are fixed in the same working tree:

| Defect | Owner | Fix |
|---|---|---|
| The Speakers rows printed a raw service-record model. | `crates/homepod-cast/src/ui/pages/speakers.rs` | Resolve through `presentation::device_class_key`, as the Overview card always did. |
| A failed session drew two buttons announcing the identical word "Retry"/"Erneut versuchen" and dispatching the identical event. | `crates/homepod-cast/src/ui/presentation.rs` | `OverviewModel::from_snapshot` drops the notice's corrective action when its label resolves to the same catalog key as the page header's lifecycle command. The consequence sentence stays. |
| `AppEvent::WindowGeometryChanged` had **no producer in the whole binary**, and the viewport was built from a hard-coded 1120x720 that ignored the restored value. The window could never remember its size. | `crates/homepod-cast/src/ui/mod.rs` (report) and `crates/homepod-cast/src/app/mod.rs` (restore) | `ControlCenterApp::report_window_geometry` dispatches once per distinct size; `app::main_viewport` builds the window from the persisted size and maximized flag. The position stays persisted-but-unrestored, deliberately: restoring an x/y from a monitor that is no longer attached puts the window out of reach, and the monitor arithmetic belongs to a later task. |

One component fixture also had to change: `app_shell`'s test receivers used a
product name where a service-record model field was expected, which resolved to
the unknown-class fallback — the same word as the Speakers destination's own
title. The fixture now uses a redacted model identifier.

**Still open in Task 10:** the SHA-256 of a signed release build, the real-HWND
`--ignored` test, and the manual acceptance run. The release build itself
succeeds, and the window has been launched, driven and photographed repeatedly
since; what remains needs a person at the machine.

Driving the built window on 2026-08-26 found seven further defects that no test
had caught, fixed in `6977093`. Three are worth remembering as a class:

* The Overview's one dominant command **disappeared** whenever nothing was
  selectable, so the header geometry jumped between states — which section 4.3
  forbids by name. It is drawn and inert now.
* A **filled, enabled, dead** Retry stood in the failed-and-nothing-selectable
  state: the most prominent control on the page dispatched an event the reducer
  discarded. Found by counting filled commands across all eight states, not by
  looking at any one of them.
* The Audio page claimed *"WASAPI reports no selectable devices"* while the
  same run's log named the device it was capturing from. The page has no data
  source at all and never asked. A fabricated measurement reads as more
  trustworthy than an admitted gap, which is what makes it the worse failure.

Supervisor relay: the control lock is no longer held across the wait for an
update (`f3d31ed`). `backend_lifecycle` went from 66.9s to 6.3s.

## Baseline as of 2026-08-26, and why the older numbers were wrong

Measured on branch `feat/acceptance`, base `b1327cc`, with the Task-10 work in
the tree. The exact commands, in this shell, with `cargo` put on the PATH
first — `--all-targets` and the bare call cover different sets (benches versus
doc-tests), so the flag is part of the number:

```
export PATH="/c/Users/Thorsten/.cargo/bin:$PATH"
cargo test -p homepod-cast --all-targets --no-fail-fast
                                            764 passed, 0 failed, 2 ignored
cargo test --workspace --all-targets --no-fail-fast
                                           1794 passed, 0 failed, 6 ignored
cargo check --workspace --all-targets       clean
```

**Historical GUI baseline, measured on `e359c6c` on 2026-08-26:**

```
cargo test --workspace --all-targets --no-fail-fast
                                           1929 passed, 0 failed, 6 ignored
                                           40 "Running", 39 "test result:"
```

**Fresh package baseline at `5b3b4bd` (run today):**

```text
cargo test -p homepod-cast --all-targets --no-fail-fast
929 passed, 0 failed, 2 ignored; 12/12 targets completed
```

The last known complete workspace run from the handoff was **1963 passed, 0
failed, 6 ignored**; it was not rerun today. The older figures above remain
historical GUI evidence.

The 40-against-39 gap is the expected `crypto_benches` one and the only one
allowed. **Never read the exit code of a pipeline for this** — compare the two
counts, or a suite that never started looks exactly like a suite that passed.

Base `b1327cc` before this task, same commands: 748 passed / 0 / 2 and,
package-only, 12 suites started and 12 summaries printed. The workspace run
starts 40 suites and prints 39 summaries; the missing one is `crypto_benches`,
a Criterion harness that prints `Testing … / Success` instead of libtest's
format. That single discrepancy is expected and is the only one allowed.

`cargo clippy -p homepod-cast --all-targets -- -D warnings` **fails on this
tree, and did before this task**: `airplay-resampler`, `airplay-crypto` and
`airplay-timing` carry pre-existing lints and clippy denies them before
`homepod-cast` is ever compiled. Run without `-D warnings`, `homepod-cast`
itself produces six warnings, all in files untouched here
(`app/snapshot.rs`, `app/state.rs`, `cast.rs`, `platform/fonts.rs`) and none in
`ui/`.

Two scans the Subproject-1 plan prescribes need correcting before they are
used as gates:

* `rg 'notice\.summary|summary\.clone\(\)' crates/homepod-cast/src/ui` matches
  three lines, none of them a leak: a doc comment stating that
  `notice.summary` is deliberately not read, and two test fixtures cloning
  their *own* localized model strings. The pattern is too broad to be a gate.
* The repaint allow-list in the plan names `src/platform/windows_settings.rs`
  and `src/ui/components/route_ribbon.rs`. The two real `request_repaint()`
  sites are `src/ui/mod.rs:103` (the snapshot waker) and
  `src/app/mod.rs:510` (the waker handed to the Windows settings monitor from
  the launch path). The list is stale, not the code.

The historical note below explains why every figure recorded before `131cd4c`
is unusable.

**Every figure recorded here before `131cd4c` is unusable.** The shutdown
coordinator armed its last-resort exit valve from a literal at the call site,
so every test that drove the real shutdown path armed a real
`std::process::exit(0)` on a background thread that is deliberately never
disarmed. Three tests did, with budgets of 6.2s, 7.1s and 10.0s. Whichever
fired first ended the whole run mid-test, with exit code 0 and no
`test result` line -- so cargo reported success and every test scheduled after
that point silently never ran. Measured on the restored fault: 165 of 168
`ok` lines, zero summary lines, exit 0, last visible line `FAILED`.

That is where the three disagreeing baselines in circulation (383 / 391 / 392)
came from: the truncation point moves with the scheduling.

**Verify integrity, not the exit code.** A run is only trustworthy when every
started suite also reported a summary:

```
grep -c "Running \|Doc-tests" out.txt      # suites started
grep -c "^test result:" out.txt            # suites that finished
```

The two must agree, except for `crypto_benches` -- a Criterion harness that
prints `Testing .. / Success` instead of libtest's format. Never read the exit
code of a pipeline: `| tee` and `| tail` both mask cargo's status, and `cargo`
itself is not on the shell PATH, so an unprefixed call exits 127 and looks
green through a pipe.

Historical state: the working tree was clean at `ac3c97f`; a backup of the
pre-handover uncommitted state was preserved as tag `wip-backup-uebernahme`.

`cargo` is **not** on the shell PATH; prefix every invocation with
`$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"`. Always wrap test runs in
a timeout - this tree has already produced two runaway loops that burned cores
for the better part of an hour.

## Open questions for the owner

The only remaining specification question confirmed by source inspection is:

1. **`LatencyConfig::sender_buffer_ms` has no protocol consumer.** The field is
   configured, but the transport deliberately does not guess it onto the SETUP
   latency window or `StreamConfig::latency_max`; a regression test pins that
   behavior. Decide whether the field should gain a defined protocol meaning or
   be removed from the public configuration.

Resolved historical questions:

- Render lead for the single-survivor path and the `run_intent`/Stop reconcile
  behavior were fixed in `4ee603e`.
- The network-flake/host-monitor finding was clarified by `fdf3170` and
  `cf7fc5c`, including the test-rig monitor-subscription fix.
- The wake-gate race was addressed in `865522e`.

## Remaining validation and precision gaps

Both are larger than anything the UI work can close, and both should be read
before any further feature is planned.

1. **The connected resilience path still needs manual validation.**
   `start_device_backend` (`crates/homepod-cast/src/backend/controller.rs:291`)
   is wired through `2818f01`, `932c7fc`, `0df27bc` and completed recovery work
   in `a8d07d8`; recovery is visible in the shipping application. Manual
   release gates still cover rejoin, primary-device loss and network changes.
2. **Two speakers at once is confirmed working** (2026-08-26). The remaining
   timing work -- gPTP peer delay -- is precision, not function.


## Hardware gates the owner must run

Nothing below can be verified by an agent, and no agent may play audio through
the speakers unattended:

- that pairing, PTP BMCA yield, SETPEERS and RTP fan-out actually complete
  against real HomePods - only the translation layer is covered by tests;
- **owner-only primary-loss gate:** while a group with at least two receivers is
  running, make the active primary receiver deliberately unreachable; record
  the deterministic survivor rebuild, continued test tone (or the honest
  resulting error state), and bounded teardown;
- that the concurrent group teardown holds the 4 s budget in practice;
- 30-minute stability, sleep/wake, network interruption, receiver rejoin and
  repeated start/stop leak testing, all still outstanding from
  [`VALIDATION.md`](VALIDATION.md);
- the twelve UI checks A1-A12 in [`VALIDATION.md`](VALIDATION.md) that read
  pixels, screen readers, DPI, the tray, and the real window manager.

The legacy implementation is separate on branch `legacy/homepod-cast-basics`
and tag `homepod-cast-basics-v1`.

## Non-negotiable safety rules

- Never run `git reset --hard`, `git checkout --`, `git clean`, broad restore, or a repository-wide stash.
- Use `apply_patch` for source and documentation edits.
- Never run mutating `cargo fmt --all`; format and check only task-owned Rust files.
- Preserve `AppHandle::{dispatch,snapshot,drain_ui_effects,install_waker}`.
- No address, socket, secret, raw endpoint ID, `Device`, connection, session, or backend diagnostic object may enter UI-facing snapshots, events, or renderers.
- Preserve stable `DeviceId`/`ReceiverId` identity across backend boundaries; presentation-only aliases and keys must be explicitly mapped.
- Keep backend/UI work non-blocking and bounded. Full queues drop/coalesce only according to their documented policy.
- Diagnostic/export data is allow-listed and support-safe; never log packet, audio, secret, raw identity, or raw backend error material.
- Do not add Claude/Codex/AGENTS/ADVISOR files or assistant co-author trailers. Existing ignore rules are binding.
- If available quota reaches 15%, finish the current safe checkpoint, update this handoff, and stop before another task.

## Definition of done for each implementation task

- [ ] Read the complete binding task before editing.
- [ ] Confirm exact task-owned files and current unrelated dirty paths.
- [ ] Add a focused test first and record the expected RED reason.
- [ ] Implement the minimum behavior needed for GREEN.
- [ ] Run focused tests, affected package tests, and the task's build/check commands.
- [ ] Run direct `rustfmt --check` only on task-owned Rust files.
- [ ] Run `git diff --check`.
- [ ] Stage only the explicit task files and inspect `git diff --cached --name-status`.
- [ ] Commit with the binding plan's message.
- [ ] Dispatch an independent read-only reviewer over the complete task base-to-HEAD diff.
- [ ] Fix and re-review every Critical/Important finding.
- [ ] Record test evidence and the reviewed commit range.

## Historical execution authorization (superseded)

The following phase/stoppage text is retained only as historical handoff
context. It is not the current authorization or next-step order above.

### Historical Phase 1 — Windows Native Foundation and Command Home

At that historical point, this was the only authorized implementation phase.

1. Read the full [Subproject-1 plan](superpowers/plans/2026-08-24-openaircast-windows-native-01-foundation-command-home.md).
2. Execute one task at a time using TDD and exact-file commits.
3. Keep the active backend/calibration paths untouched and unstaged.
4. Complete the plan's automated matrix, release build, native launch, accessibility/manual checks, validation record, executable SHA-256, and independent final review.
5. Record every Subproject-1 commit from the implementation base through reviewed HEAD; fix commits are allowed and must be included.
6. Emit the hard-stop line and stop.

### Historical STOP — fresh user approval required below this line

The phases below are an ordered roadmap, not current authorization. They must not be started merely because this file is being followed.

### Historical Phase 2 — finish capability prerequisites and Windows Native capability pages

After a fresh user instruction to continue:

1. Finish and review the backend prerequisites named by the [Subproject-2 plan](superpowers/plans/2026-08-24-openaircast-windows-native-02-capability-pages.md), including resilience Tasks 10 and 15–17 where still open.
2. Execute Subproject 2. It owns the bounded shell/capability bridge and replaces the old resilience Task-18 shell-integration surface.
3. Run the Subproject-2 failure-storm, accessibility, and capability acceptance gates.
4. Stop at the Subproject-2 checkpoint unless the user explicitly authorizes Subproject 3.

Do not repeat already completed resilience Tasks 7, 9, 13, or 14.

### Historical Phase 3 — diagnostics, calibration, and release UI

After a fresh user instruction to continue and after Subproject 2 passes:

1. Finish and review diagnostics/calibration Tasks 9–11 where still open.
2. Execute the [Subproject-3 plan](superpowers/plans/2026-08-24-openaircast-windows-native-03-diagnostics-release.md). It is the GUI authority for Diagnostics pages, 4 Hz visibility-gated refresh, calibration presentation, export/copy actions, truthfulness, and final Windows Native acceptance.
3. Treat the old Windows Calm diagnostics UI instructions as superseded; do not implement a second diagnostics UI or polling path.
4. Complete the final integrated release and hardware gates recorded in the binding plans and `docs/VALIDATION.md`.

## Final repository audit

```powershell
git status --short --branch
git log --oneline --decorate -30
git ls-files | rg -i '(^|/)(claude|codex)(\.md|/|$)|(^|/)AGENTS\.md$|(^|/)ADVISOR\.md$'
git log dcb32e6..HEAD --format='%B' | rg -i 'co-authored-by:.*(claude|codex)'
rg -n "WebView2|Qt|proprietary|per-seat|hosted service" crates/homepod-cast README.md THIRD_PARTY_NOTICES.md
```

Current expected handoff: only explicitly scoped source/docs committed, unrelated
changes preserved, no assistant attribution, and no push, merge or publication.
