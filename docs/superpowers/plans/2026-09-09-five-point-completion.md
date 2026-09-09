# Five-point completion implementation plan

> For agentic workers: REQUIRED: superpowers:subagent-driven-development, test-driven-development and verification-before-completion. Work serially in the existing isolated worktree. Review each task independently before the next implementation.

**Goal:** Secure the user-confirmed three-speaker playback and individual levels, apply safe levels before audio, finish everyday controls and saved groups, and run honest release checks.

**Architecture:** Preserve Rust/egui and the controller/session/transport boundary. Backend owns durable applied state; UI owns only drafts. All device I/O remains outside painting. Use existing AirPlay APIs, not a new protocol. Approved design: `docs/superpowers/specs/2026-09-08-openaircast-complete-product-design.md`; visual contract: `docs/UI_WINDOWS_NATIVE_2026-09-08.md`.

**Global constraints (apply to every task):** Preserve existing work. No push, merge, release publishing, secret changes or network/standby disruption. Never terminate the running app without permission. Stop at 15% remaining quota. Use apply_patch, scoped rustfmt, offline Cargo. No child agents. No invented hardware evidence. Commands, errors and persisted values must reach the UI truthfully; no optimistic durable success. DE/EN, keyboard and high contrast remain supported. Do not add AI attribution. Read AIRPLAY_2_SPEC.md before changing protocol details.

**Five user points:** (1) task 1; (2) task 2; (3) tasks 3–4; (4) tasks 5–6; (5) task 7. Existing approvals authorize this continuation; this plan does not claim the entire version 1.0 roadmap will be finished.

## Task 1: Secure the confirmed baseline

Files: current dirty Rust feature files listed by `git status --short`, `docs/NEXT_STEPS.md`, `docs/UI_WINDOWS_NATIVE_2026-09-08.md`, approved product spec, existing multiroom plan and this plan.

1. Record owner confirmation: all three speakers play; individual volume works. Remove current instructions to repeat that already completed listening test. Keep long-run, measured skew and pre-audio level gates open.
2. Run `cargo test --offline --workspace --quiet`, `git diff --check`. Record actual counts and blockers. Do not claim fresh build success if the running exe blocks it.
3. Stage only the known feature source and documentation paths (not target, credentials, agent files or unrelated additions). Commit as `fix: preserve verified multiroom playback and receiver levels`.
4. Acceptance: confirmation docs agree, original behavior preserved, tests reported, baseline commit exists. No feature implementation in this task.

## Task 2: Apply receiver volumes before streaming

Files: `crates/homepod-cast/src/backend/{controller,session,transport}.rs`, existing fake factories in tests and backend lifecycle/recovery tests as required.

1. Add a failing transport ordering test: connected receivers must receive effective master × receiver level (or zero when muted) before either single or group live-stream start is invoked.
2. Carry initial effective levels through session setup. Preserve current values on retries and recovery; account for level changes during an in-flight connection using a latest-value channel or equivalently bounded synchronized state. Do not reset levels to unity and correct them after audio begins.
3. Apply volumes after connection, before starting audio. A failed initial volume operation must not start an unsafe stream; use bounded error/cleanup handling and redacted user-visible failure.
4. Keep the existing live-volume path, persisted-save-before-echo behavior and no session restart on a slider change. Avoid changing wire formats.
5. Test single/group ordering, master multiplication, mute, partial connection, initial-volume failure, recovery and a changed value during delayed connect. Run transport/session tests and `cargo test --offline -p homepod-cast --test backend_lifecycle --test system_recovery`.
6. Commit the tested change and report exact commands/results and limitations.

## Task 3: Compact everyday UI and shared speaker selection

Files: `crates/homepod-cast/src/ui/pages/{overview,speakers}.rs`, `ui/components`, `ui/presentation.rs`, `ui/i18n.rs`, `ui/acceptance.rs`; app reducer only if required for existing selection actions.

1. Use existing global staged selection on both pages. Add behavior tests proving selection and apply feedback agree across navigation; touching a receiver slider must not toggle selection.
2. Reduce normal route panel to 88–112 logical px where content permits; preserve truthful source/selected/active statuses, one primary action and reachable stop. Keep the native palette, 176/64 navigation, existing text hierarchy and 40px control targets.
3. Reuse the existing apply bar and receiver-level components; avoid duplicate state or a second selection model. Ensure small windows scroll naturally without clipping or hidden essential actions.
4. Run existing shell acceptance tests. Extend/run the opt-in GPU render review across light/dark/high contrast and 1120×720/900×600; inspect outputs. Headless images do not prove actual Windows DPI behavior.
5. Update the detailed UI Markdown contract with dimensions and interactions, then commit.

## Task 4: Enumerate and choose real Windows playback endpoints

Files: `crates/homepod-cast/src/backend/{controller,capture,model}.rs`, `backend_bridge.rs`, `app/{event,state,reducer,snapshot}.rs`, `ui/pages/audio.rs`, `ui/i18n.rs`, existing lifecycle and capture tests.

1. Add failing tests for real enumeration publication, unknown versus measured-empty, refresh failure preserving last success, explicit missing preference and recovery.
2. Call existing `CaptureSource::endpoints()` away from the controller/UI thread using bounded single-flight work; COM open/enumeration must not block the current-thread runtime. Trigger appropriate initial and capture recovery refreshes. Never create unlimited hung workers.
3. Preserve `AudioSourceSnapshot.active_endpoints: Option<Vec<AudioEndpoint>>`: None means unmeasured; Some(empty) means successful empty scan. Reconcile an explicit preference honestly, including capture proof when the scan omits the captured endpoint.
4. Keep opaque UI endpoint keys stable and reverse-map choices via BackendPort. Show system-default separately from actual Windows loopback playback device; do not call it a microphone. Failed refreshes must not fabricate an empty device list.
5. WIP commit `469b530` may be inspected for design only, not blindly cherry-picked. Run focused fake-capture, bridge, reducer and audio-page tests; document that real endpoint switching still needs Windows verification if unavailable. Commit.

## Task 5: Saved-group and applied-selection backend-to-UI contract

Files: `backend_bridge.rs`, `device_service.rs`, `app/{event,effect,state,snapshot,reducer}.rs`, backend model/controller/command only as needed, and their tests.

1. Project existing backend saved groups and desired membership/revision into shell-safe stable-ID values (no sockets, addresses or secrets). Add CRUD/apply/apply-and-start commands through the established bridge.
2. Tests first: a clean staged selection follows a new applied revision; a dirty draft survives external changes and is marked stale. Persisted group data comes from backend echo, never optimistic writes.
3. Correlate operation results with request/target so busy, validation, persistence and closed-channel failures are visible and do not leave permanent saving state. Retain outcomes in snapshots where required to survive broadcast lag; do not change the four AppHandle methods.
4. Preserve existing backend validation and atomic activation of member IDs and receiver levels. Group save must not start streaming; delete must not stop current playback.
5. Test round-trip commands, accepted/failed results, durable reload, offline members, stale drafts and apply/apply-start separation. Run affected tests and commit.

## Task 6: Saved groups UI

Files: `ui/pages/groups.rs`, `ui/i18n.rs`, `ui/presentation.rs`, `ui/acceptance.rs` and group-specific app draft fields/tests if needed.

1. Replace placeholder with saved group list, create/edit form and explicit actions. Editor draft is distinct from global speaker selection. Preserve offline members and levels when editing.
2. Provide name validation, member selection and receiver levels; saving persists only. Expose distinct Apply and Apply & start actions. Delete requires confirmation naming the exact group.
3. Render pending/error/result feedback from task 5. Keyboard navigation and localized accessible controls follow the native design; existing active playback remains visible and undisturbed by editing/deleting.
4. Tests: create/edit/delete, cancel without mutation, save does not start, apply vs start, failure preserves draft and offline member retention. Render and inspect representative empty/list/editor/error screens; update UI contract. Commit.

## Task 7: Release checks and handoff

Files: release checks under existing `crates/homepod-cast/tests`, `docs/NEXT_STEPS.md`, native UI contract and `docs/RELEASE_CHECK_2026-09-09.md`.

1. Run full offline workspace tests, scoped Clippy, `git diff --check`, and build the Windows app when it is not locked. Record actual counts, warnings and executable path.
2. Exercise deterministic existing recovery, start/stop and disconnect tests; extend missing targeted regression coverage where practical using test-first changes. Do not substitute fake tests for real listening/standby evidence.
3. Obtain owner approval before disrupting standby/network or starting hardware audio. If unavailable, record those gates as explicitly pending with a short executable checklist (30-minute multiroom playback, independent levels and restart, disconnect/rejoin, standby/resume, tray exit/relaunch). Do not mark a 1.0 release ready.
4. Update all five-point statuses, commits, evidence and precise remaining manual gates. Run final independent whole-branch review, address findings, and commit final documentation. No push or publish.

## Verification and completion

Each implementation has fresh test evidence and one independent review before proceeding. Final status separates implemented software, automated checks, owner-confirmed listening, and unperformed physical release gates. A hardware blocker only blocks that gate, not other safe work. Any scope reduction or quota pause must be explicit in the handoff.
