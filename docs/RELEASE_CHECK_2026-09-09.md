# OpenAirCast — release check, 2026-09-09

> Historical test/build evidence for the revisions named below. Absolute paths
> in old test records identify that developer's test environment; they are not
> installation instructions. Use [distribution](DISTRIBUTION.md) for current
> output locations and public-release status.

> **Owner decision after review:** The owner explicitly authorizes commit,
> merge, push and release preparation despite the documented Suspend/Cancel
> defect. The earlier technical recommendation against merge is retained below
> as review history, not the current integration instruction. The issue remains
> open and accepted; physical/native/packaging/security checks not performed
> remain unverified. See [release notes](RELEASE_NOTES_2026-09-09.md).

## Main-checkout integration and cleanup

The GUI worktree was fast-forwarded into its original product branch
`openaircast/main` (`8e04492` → `74c02e3`) in the main checkout. A fresh complete
test run from that checkout passed **2,104 tests, 0 failures, 15 ignored**,
exit 0, all 44 targets including doctests. Compilation took 4m 08s; the shell
tests took 234.03s. Existing warnings remain. The command used offline/locked,
two jobs, the Windows MSVC target, two test threads and BelowNormal priority;
the old worktree's target directory was reused only as a build cache before
retirement. No application source changed after this test.

Cleanup removes three tracked `.DS_Store` files and updates current handoff
paths. It does not remove the accepted Suspend/Cancel defect or perform native
playback/standby tests. Current artifacts are in the main checkout's `dist/`,
and the cleanup/source-export outcome is recorded in
[integration notes](INTEGRATION_2026-09-09.md).

## Optimized release preparation after owner acceptance

Application source remains `047dfc6070cf42d8404aca779dd17c57e5af6358`;
release notes and the matching source archive use commit
`802551f8fea1874453e2ca0c3e8d33ad1d458383`. Subsequent preparation changes only
documentation/artifacts, not application behavior. The new optimized Windows
build completed successfully (exit 0, 12m 01s), with existing warnings:

```powershell
(Get-Process -Id $PID).PriorityClass='BelowNormal'
cargo build --offline --locked -j 2 --target x86_64-pc-windows-msvc --release -p homepod-cast --bin openaircast
```

- File: `target/x86_64-pc-windows-msvc/release/openaircast.exe`.
- Size: 23,602,688 bytes; write time: 2026-09-09 05:52:33 UTC.
- SHA-256: `7F155CD26E49C61694C2B04DD1C3ED37E6677D2AA60C5064B8D262E35533C599`.
- PE inspection: AMD64 (`0x8664`), Windows GUI subsystem (`2`).
- Authenticode: NotSigned. No native launch or playback test was performed.
- Local package: `dist/OpenAirCast-0.1.0-windows-x64-20260909.zip`;
  matching source: `dist/OpenAirCast-0.1.0-source-20260909.zip`.

Fresh full workspace after release compilation: **2,104 passed, 0 failed,
15 ignored**, exit 0, all 44 targets including doctests completed. Compilation
took 29.61s; the shell's 741 passing tests took 230.38s (4 ignored). Command:
`cargo test --offline --locked -j 2 --target x86_64-pc-windows-msvc --workspace -- --test-threads=2`,
under BelowNormal priority, after the release build, with no concurrent Cargo.
Application source was unchanged. This test run does not cover the known
Suspend/Cancel defect or replace unperformed native/physical acceptance.

The canonical `dist/OpenAirCast.exe` matches the release hash above. Its previous
version was preserved as `dist/OpenAirCast-previous-d52851ee.exe`, SHA-256
`D52851EEBCEBEB04C6F5DFFE9560FBA342DC5B4EA400DC146291F9A2C22E8F51`.

This is the optimized release artifact, not the debug executable described in
the historical verification below. Target-branch confirmation and completed
merge/push evidence are separate from a successful local release build.

The approved five-point software work is implemented on
`codex/five-point-completion`. This record separates automated evidence,
historical owner listening evidence and unperformed physical release gates.
It does not approve a 1.0 release, signing, publishing, pushing or merging.

## Software scope and commits

| Scope | Implementation | Evidence / limit |
|---|---|---|
| Preserve verified multiroom and individual levels | `113984b`, `4ce6b7d`, `c4b8ebe`, `08d83c8`, `a5136e6` | Existing behavior preserved; production transport start applies effective levels before audio and later queued updates win. Deterministic transport coverage; no fresh listening run |
| Compact route and shared speaker selection | `1ac5c31` | 108-point normal route fixture, shared draft, independent level controls, fixed lifecycle header and change bar; synthetic GPU and interaction coverage |
| Real Windows playback endpoints | `951aa87`, `e9b7dde` | Bounded off-thread production enumeration, unknown/empty distinction, retained last success with localized refresh failure, scanner termination and shutdown coverage |
| Saved groups backend and UI | `21af72b`, `4373e6b`, `38d3bc8`, `9eebc0e` | Confirmed templates, exact command receipts, isolated local editor, explicit apply/start/delete, retained observed result or ConfirmationLost after navigation |
| Release integration | `b639d30`, `e720ff5`, `3595057` | Group start from Idle/Stopping exposes Starting/Cancel; exact receipts retain the backend attempt floor after success, so predecessor Stop/Active/Failed cannot settle the new Start; later Stop wins within a batch; failed level persistence cannot start an otherwise matching group |

Tasks 1–6 and the two historical Task 7 rounds received scoped reviews.
Whole-branch review of `8e04492..e2119ba` found two Important volume defects
and one Minor offline-editor defect. The single consolidated correction wave
at `047dfc6070cf42d8404aca779dd17c57e5af6358` closes all three: the release gate
now reaches actual audio dispatch, confirmed master volume is restored into
the shell, and offline members remain reselectable. Scoped re-review over
`e2119ba..047dfc6` marked those findings addressed; no Critical finding was found.

**Software is NOT merge-ready. One Important residual remains OPEN.** Root
inspection confirmed that `session.rs`'s `Superseded(Suspend)` cleans up the
prepared transport and then replays Suspend with neither active nor in-flight
ownership. `apply_inline_request` consequently omits Stopped. If the owner
cancels after wake but before the two-second resume settle, reconcile returns
while suspended and `finish_resume` only handles Running: the shell can remain
Stopping. Next source work must add a failing **Suspend during activation →
Cancel before resume settle** regression and preserve the prepared generation's
Stopped publication after bounded cleanup, followed by focused review and
fresh verification. Existing green tests do not waive this missing regression.
The final correction-wave cap is reached; this tail changes documentation only,
not source/tests. Branch, worktree and scratch evidence are retained.

## Current verification on `047dfc6`

Source remained `047dfc6070cf42d8404aca779dd17c57e5af6358` throughout this
verification-only tail. All three Cargo commands ran strictly serially in
BelowNormal PowerShell processes, offline, with two build jobs and the explicit
Windows MSVC target; tests used two threads. No source/test edits, native app
launch, playback, hardware action or process termination occurred.

| Check | Fresh result |
|---|---|
| Full workspace including doctests | **2,104 passed, 0 failed, 15 ignored**, exit 0, 44 targets; compilation 2m 30s |
| App targets within that total | Shell 741 passed/4 ignored (231.09s); library 142; backend lifecycle 36; system recovery 35; session recovery 18; general recovery 8 |
| Audio/client libraries within that total | Audio 176 passed/2 ignored; client 111 passed |
| Scoped Clippy | Exit 0, 21.47s; 63 diagnostic warning headers and 8 target summaries |
| Normal Windows debug build | Exit 0, 43.98s; 57 diagnostic warning headers and 6 target summaries |

The workspace log has 97 diagnostic warning headers and 19 target summaries.
Clippy retains the same six app diagnostics: sort_by_key in `snapshot.rs`, two
derivable defaults in `state.rs`, clone-on-Copy and large enum in `cast.rs`,
and needless return in `fonts.rs`; dependency warnings remain. No warning-free
claim or unrelated warning repair is made. Ignored real-device/real-HWND,
opt-in GPU and doctest cases remain unperformed. In particular, the missing
Suspend-activation/Cancel-resume-settle regression is not covered by the green
full run: the Important residual above still blocks software merge readiness.

```powershell
(Get-Process -Id $PID).PriorityClass='BelowNormal'
cargo test --offline -j 2 --target x86_64-pc-windows-msvc --workspace -- --test-threads=2
cargo clippy --offline -j 2 --target x86_64-pc-windows-msvc -p homepod-cast --bin openaircast --tests --no-deps --message-format short
cargo build --offline -j 2 --target x86_64-pc-windows-msvc -p homepod-cast --bin openaircast
```

Fresh unsigned debug artifact, **not launched**:

- Path: `target/x86_64-pc-windows-msvc/debug/openaircast.exe` in the implementation worktree.
- Exact local path: `C:/Users/Thorsten/Documents/Claude/Projects/HomePodCast/.worktrees/gui-control-center-implementation/target/x86_64-pc-windows-msvc/debug/openaircast.exe`.
- Size: 63,960,064 bytes; last write: 2026-09-09 04:06:25 UTC.
- SHA-256: `55201E6BEB941BBE07F0957C5A23744DFE2D8DC7C0BD7142B069E0B5B71B12A6`.

Full ignored scratch logs are retained in
`.superpowers/sdd/2026-09-09-five-point-completion/` as
`final-verification-workspace.log`, `final-verification-clippy.log` and
`final-verification-build.log`; `final-fix-report.md` records the source wave's
RED/GREEN evidence and review disposition. That earlier wave briefly queued a
second targeted Cargo behind the first Cargo's build lock; it did not compile
concurrently. Every verification-tail Cargo command was strictly serial.
The two metadata-only source flags remain untouched and unstaged. This record
does not approve a merge, push, publication, release or scratch cleanup.

## Historical automated checks on `3595057` (not current verification)

All Cargo commands run from the worktree with the calling PowerShell process
set to BelowNormal, offline, two build jobs, the Windows MSVC target and two
test threads. No competing Cargo invocation, native app or physical audio
session was started by the release-check implementer.

The three required integration regressions first failed against the unchanged
implementation: group-start→Stop ended Running; failed group-level storage
with already-matching member IDs also ended Running; the shell's group start
remained Stopped and offered no Cancel. After the bounded fixes, these pass.
Additional cases prove that Stop→later-group-start wins in the other direction,
exact failure cannot overwrite a later Stop generation, group success is not
audio delivery, and the actual Groups Cancel button dispatches Stop.
An explicit group start during Stopping also receives a fresh generation;
its regression first observed Stopping7 instead of Starting8, then verified
that old SessionStopped7 is ignored and Cancel advances to Stopping9.

Focused app/bridge/groups result: **220 passed, 0 failed, 1 ignored**.
Expanded integration filters: **10 passed, 0 failed** across the binary,
backend lifecycle and system recovery test targets (including two matching
preexisting binary tests).

The pre-final workspace run failed: **2,080 passed, 1 failed, 8 ignored**
before Cargo stopped (doc tests not reached). The one stale latency test
assumed a rejected command could not publish any snapshot revision. Exact
command outcomes now deliberately do so; the corrected test requires the
specific Rejected outcome and unchanged latency, membership, run intent,
session, receiver/master levels and mute, while retaining no-restart checks.
The focused rerun passed all **8 binary and 2 system-recovery tests**.

Historical full workspace on reviewed-fix source `3595057`: **2,092 passed, 0 failed,
15 ignored**, exit 0, across 44 test targets including doc tests. The app binary
has 736 passed/4 ignored; app library 138 passed; backend lifecycle 35 passed;
system recovery 35 passed; session recovery 18 passed; general recovery 8 passed. Existing deterministic
disconnect/rejoin, start/stop, suspend/resume and startup-volume scenarios ran.
Ignored cases remain unperformed by this run; they include real-device/real-HWND
and opt-in GPU checks as well as ignored doctests.
Compilation took 1 minute 32 seconds; the app binary tests took 229.79 seconds.

```powershell
(Get-Process -Id $PID).PriorityClass='BelowNormal'
cargo test --offline -j 2 --target x86_64-pc-windows-msvc --workspace -- --test-threads=2
```

Scoped Clippy after the final review fix: **exit 0**, 11.55 seconds. The log contains 63 warning diagnostic
headers plus 8 target-summary lines. Six existing app warnings remain:
`snapshot.rs` sort_by_key, two derivable defaults in `state.rs`, clone-on-Copy
and large-enum warnings in `cast.rs`, and needless return in `fonts.rs`.
Remaining diagnostics concern existing dependencies. No warning points to
the changed Task 7 logic. This is not a warning-free build claim.
The workspace test log contains 95 warning diagnostic headers plus 19 target
summaries, including unused/dead-code, deprecated pairing and an example's
shared-reference-to-mutable-static warning; these were not silently repaired.

```powershell
(Get-Process -Id $PID).PriorityClass='BelowNormal'
cargo clippy --offline -j 2 --target x86_64-pc-windows-msvc -p homepod-cast --bin openaircast --tests --no-deps --message-format short
```

Exact-file rustfmt checks with `--edition 2021 --config skip_children=true`
and `git diff --check` passed. Implementation/regressions are committed in
`b639d30df4f26be7e2d0d6df5fe46ae5ca3c9f88`, followed by bridge review fix
`e720ff503b5fcee30a1bf4a4245f8a80af01b7d1` and causal-attempt correction
`35950579ef1d72fa45e1f73a755330be6b6ca0ef`; source was unchanged throughout
the final full test run. The two preexisting metadata-only dirty flags on
`airplay-client/src/lib.rs` and `airplay-timing/src/traits.rs` have no content
diff and remain untouched/unstaged.

The independent Task 7 review found that the original Stopping→group-start
test bypassed the real projection producer: changed backend content could be
stamped with the new shell generation and falsely finish it. Two new RED tests
reproduced a predecessor Stop becoming Stopped8 and old Active becoming
Streaming8 before the exact group receipt. The fix reuses the existing receipt
and revision barrier; every unanswered generation requires a matching phase,
while settled generations retain spontaneous health/stop updates. Focused
regressions passed **6/6**; covering app/bridge/groups passed **223**, with
**0 failed/1 ignored**, before that round's full run. No receipt success
itself asserts audio delivery. This was the first fix round, not the final
correlation guarantee: its post-success receipt still admitted predecessor
Active/Failed phases.

Second review reproduced both post-success failures as RED: old Active became
Streaming8, and old Failed ended the new Starting8. The existing exact receipt
now carries the backend session-generation floor assigned after batch
reconciliation. The bridge retains ConfirmedGroupStart until a matching/newer
attempt supplies actual phase evidence. A no-op may reuse the current attempt;
a suspended start requires the next attempt. Real controller/supervisor tests
prove no-available-receiver and preflight-failure Recovering outcomes, no-op reuse and
suspend/resume. Recovery remains cancellable without assuming Starting was
observed; storage success never proves audio. Newer Cancel and settled
spontaneous health/stop updates remain covered. Before scoped approval, the
combined covering run passed **249 tests, 0 failed, 1 ignored**, plus **2/2**
targeted persistence/identical-selection checks. That historical Task 7 source was
`3595057`; the earlier 2,084-pass run belongs only to `e720ff5`.

Historical Windows debug build of `3595057`: **exit 0**, 31.20 seconds. It is unsigned
debug output, not the historical `dist` executable or a release package.
The build log has 57 existing dependency warning diagnostic headers plus
6 target-summary lines; it is not warning-free.

```powershell
(Get-Process -Id $PID).PriorityClass='BelowNormal'
cargo build --offline -j 2 --target x86_64-pc-windows-msvc -p homepod-cast --bin openaircast
```

- Executable: `target/x86_64-pc-windows-msvc/debug/openaircast.exe`.
- Exact local path: `C:/Users/Thorsten/Documents/Claude/Projects/HomePodCast/.worktrees/gui-control-center-implementation/target/x86_64-pc-windows-msvc/debug/openaircast.exe`.
- Size: 63,914,496 bytes; last write: 2026-09-09 03:01:08 UTC.
- SHA-256: `987F0784038451A34EBE063CB80AF6C85D3A46C29F1CF22D8D1BC99DAD801E07`.
- This refreshed executable has not been natively launched in the release check.

## Native UI check

This native check applies specifically to the earlier `b639d30` build with
SHA-256 `581A35160F8C085F046FF368ABC2F7D5758EFBF610BD0061F1D437D0A7D6289F`.
It is not a native check of later review fixes, including current source `047dfc6`.

The root operator verified `auto_connect: false`, no running OpenAirCast
process, and the earlier `b639d30` hash immediately before launch. Launching
that exact earlier executable succeeded: a main window titled `OpenAirCast`
appeared, together with a console-titled sibling for the same executable.

Screenshot capture failed twice, including a fresh window selection retry:
`SetIsBorderRequired failed: Schnittstelle nicht unterstützt (0x80004002)`.
Separate text-only Windows UI Automation succeeded but exposed only the window,
title bar and system-menu/minimize/restore/close controls. Activating the window
did not expose app-page controls. No blind click/key sequence or helper
workaround was attempted.

**Earlier native launch: verified. Native visual/navigation/accessibility acceptance:
blocked and unperformed, not passed.** No playback, settings/group/level
mutation, standby or network disruption occurred. The app was left open in
the foreground for the owner at the end of this check; future running state
must be checked afresh. Native launch alone does not prove the normal-user
Windows 10/11, DPI, NVDA or tray matrix.

While validating the review fix, the first workspace command could not replace
the running old executable (Windows access denied, error 5) and exited before
tests ran. Root verified its hash and safely renamed only that generated file
to `target/x86_64-pc-windows-msvc/debug/openaircast-ui-check-b639d30.exe`.
Its bytes remain unchanged and the process was not terminated. Fresh output
uses the normal `openaircast.exe` path. The owner must explicitly Quit and
relaunch to run that new file; the earlier running process does not acquire
the fix through rebuilding. No further native automation attempt was made.

## Historical listening evidence

The owner confirmed simultaneous playback on three speakers and functioning
individual receiver levels on 2026-09-09. This closes the previously reported
silence regression for that run. It does not establish a fresh listening run
of this executable, 30-minute stability, quantitative inter-speaker skew,
standby recovery or pre-first-audio level behavior on physical receivers.

## Pending physical checklist — owner authorization required

Before starting, identify the intended executable and device group, save current
preferences, and ensure no competing sender session is running. Record Windows
version/build, privileges, endpoints, receiver firmware, timestamps and observed
results without publishing private device identifiers.

1. **30-minute multiroom:** Select the intended receivers, start known audio
   together, run for at least 30 minutes, and record dropouts, drift, measured
   skew, CPU/memory and recovery events. Do not infer synchronization from a
   screenshot or an active status alone.
2. **Independent levels and restart:** Change each receiver independently with
   master fixed; verify the other levels and membership remain unchanged. Stop,
   restart and verify stored balance from the first audible output. Exit and
   relaunch explicitly, check saved group/membership/levels, and repeat with the
   intended source. Record any burst or overwritten level.
3. **Disconnect/rejoin:** With explicit permission, remove one secondary
   receiver from the session's network, observe remaining playback and retry
   state, restore it and verify rejoin and balance. Repeat primary loss only
   if specifically authorized; capture the expected controlled recovery.
4. **Standby/resume:** With explicit permission, suspend and resume Windows
   during the intended session. Verify bounded shutdown, honest UI state,
   recovery, preserved levels and functioning Stop. Repeat source removal and
   default-endpoint change with the actual Windows devices when authorized.
5. **Tray exit/relaunch:** Inspect the real tray context menu, show/hide the
   window, invoke Quit, verify process/worker exit, and relaunch the same build.
   Check persisted settings/groups and absence of unintended playback. Exercise
   Tab/Shift-Tab, Enter/Space, NVDA and 100%/150% DPI on Windows 10/11 as separate
   native acceptance cases.

These gates remain **pending**, not failed and not replaced by fake-device
tests. Lack of hardware authorization blocks these actions only.

## Broader 1.0 gates still open

- Measured synchronization and long-run stability. Full gPTP peer delay is
  conditional precision work and becomes release-blocking if the current
  synchronization gates fail; unreviewed `998f817` is not included here.
- Normal-user Windows 10/11, clean-machine, DPI, accessibility/NVDA, real tray
  behavior, idle/hidden performance and extended physical stability.
- Current packaging and reproducible release pipeline, third-party notices and
  license audit, signing and security/pairing review. Existing unsigned debug
  output and historical `dist/OpenAirCast.exe` are not release artifacts.
- Remaining product-plan work beyond these five points, including receiver
  retry presentation, full auto-connect settings, Windows autostart and the
  complete diagnostics/calibration workflow.

The [current handoff](NEXT_STEPS.md), [native UI contract](UI_WINDOWS_NATIVE_2026-09-08.md)
and [approved product plan](superpowers/specs/2026-09-08-openaircast-complete-product-design.md)
describe the same scope and preserve these release limits.
