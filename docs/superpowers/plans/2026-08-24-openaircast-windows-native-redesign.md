# OpenAirCast Windows Native Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the current visually inconsistent Control Center with the approved Windows Native interface, expose the already-typed product capabilities without leaking backend objects into views, and finish with a localized, accessible, support-safe Windows release.

**Architecture:** Implementation is split into three independently reviewable subprojects. Subproject 1 delivers a startable daily-control product on the frozen `AppHandle` boundary; Subproject 2 projects the resilient device backend into functional Speakers, Groups, Audio, and Settings pages; Subproject 3 integrates diagnostics/export and runs the final release matrix. The reducer remains the sole state owner, renderers consume immutable presentation models, and all I/O stays off the eframe thread.

**Tech Stack:** Rust 2021 (MSRV 1.95), egui/eframe 0.36.1 with wgpu and AccessKit, egui_kittest, Tokio, serde/serde_json, windows-sys 0.59, ArcSwap, Inter Variable fallback, IBM Plex Mono, native Windows title bar.

**Spec:** `docs/superpowers/specs/2026-08-24-openaircast-windows-native-redesign-design.md`

## Global Constraints

- The linked 2026-08-24 design specification supersedes the earlier Windows Calm shell plan for appearance, page composition, localization, component contracts, and action priority. Earlier resilience and diagnostics specs remain authoritative for domain behavior.
- Preserve `AppHandle::{dispatch,snapshot,drain_ui_effects,install_waker}` exactly. Extending `AppEvent`, `AppEffect`, `AppState`, and `UiSnapshot` is allowed; passing `Device`, sockets, sessions, secrets, paths, addresses, or mutable backend objects into views is not.
- Preserve internal and persisted `Page::Home`; only the visible destination label changes to Overview / Übersicht.
- Keep all device work, preferences I/O, support export, endpoint enumeration, and diagnostics collection off the eframe thread. The normal UI takes at most one application snapshot per frame.
- Every behavior change is test-first: add a focused failing test, run and record the intended RED result, implement the smallest production change, run the focused GREEN command, then run the affected package suite.
- Do not use unconditional repaint loops. Only OS/input events, snapshot wakes, one bounded transition, and the visible diagnostics 250 ms schedule may request repaint.
- Do not display raw `UserFacingError`, command-failure reasons, device identity, network identity, user paths, packet/audio material, keys, pairing values, or transport objects. UI copy comes from German/English localization keys and presentation-safe named arguments.
- Use exact Light/Dark semantic tokens and Windows system colors for High Contrast from the approved spec. Body/secondary/status text must meet 4.5:1; focus and non-text state indicators must meet 3:1.
- Keep all interactive targets at least 40 logical points high and navigation/receiver destinations at least 44/56 points as specified. Custom graphics require equivalent text and AccessKit semantics.
- Use `apply_patch` for source and documentation edits. Do not add Claude, Codex, AGENTS, ADVISOR, transcript, prompt, or assistant attribution files, and do not add co-author trailers.
- Do not touch, stage, stash, reset, or reformat unrelated dirty backend/calibration work. Stage exact task paths only. Never run mutating workspace-wide formatting; format task-owned Rust files directly and use workspace formatting only as a read-only audit.
- Each implementation task ends with one focused commit and an independent read-only review. Resolve all Critical and Important findings before the next task.
- Stop after Subproject 1 is complete, review-approved, and manually startable. Do not begin Subproject 2 until the user explicitly continues.

Run from the implementation worktree in PowerShell. Resolve Cargo once per session:

```powershell
$cargo = if (Get-Command cargo -ErrorAction SilentlyContinue) {
    (Get-Command cargo).Source
} else {
    Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
}
& $cargo --version
```

Expected: Cargo/Rust 1.95 or newer.

## Plan Suite

| Order | Subproject | Deliverable | Binding plan | Start gate |
|---:|---|---|---|---|
| 1 | Native Foundation and Command Home | Startable bilingual Light/Dark/High-Contrast GUI with the complete daily receiver/volume/start-stop workflow | `docs/superpowers/plans/2026-08-24-openaircast-windows-native-01-foundation-command-home.md` | Current shell Tasks 1–14 are present; preserve the dirty resilience work |
| 2 | Capability Pages and Device Bridge | Functional Speakers, Groups, Audio, and Settings pages backed by resilient typed commands/snapshots | `docs/superpowers/plans/2026-08-24-openaircast-windows-native-02-capability-pages.md` | Subproject 1 approved; resilience Task 10 and Tasks 15–17 integrated and reviewed; this subproject replaces resilience Task 18 |
| 3 | Diagnostics and Release | Five diagnostics views, support-safe copy/export, complete accessibility/performance/release acceptance | `docs/superpowers/plans/2026-08-24-openaircast-windows-native-03-diagnostics-release.md` | Subproject 2 approved; diagnostics Tasks 9–11 integrated and reviewed; this subproject integrates/replaces diagnostics Tasks 12–15 |

The plans are intentionally separate. A later subproject consumes public interfaces from the prior one and must not reopen its approved visual/component contracts unless a failing integration test proves a required correction.

## Dependency Flow

```text
approved Windows Native design
              │
              ▼
Subproject 1: tokens + i18n + shell + Command Home
              │  STOP / user review
              ├───────────────┐
              ▼               ▼
resilience T10, T15–17   diagnostics Tasks 9–11
              │               │
              ▼               │
Subproject 2: capability pages│
(replaces resilience T18)   │
              │               │
              └───────┬───────┘
                      ▼
Subproject 3: diagnostics + export + release gate
(integrates/replaces diagnostics T12–T15)
```

## Shared File Ownership

- Subproject 1 owns `ui/theme.rs`, `ui/i18n.rs`, `ui/layout.rs`, `ui/components/*`, the shell/page dispatcher, Overview, shell preference v2, and Windows settings observation.
- Subproject 2 is the sole replacement for resilience Task 18. It owns the device-to-shell presentation adapter and functional Speakers, Groups, Audio, and connection settings. It consumes Subproject 1 components rather than restyling them and does not wait for or re-run a separate Task-18 implementation.
- Subproject 3 integrates/replaces diagnostics Tasks 12–15. It owns the single-registry application integration, diagnostics window state, diagnostics page renderers, wiring of the support copy/export commands already produced by diagnostics Task 11, final localization/accessibility matrices, and release evidence. Diagnostics Tasks 9–11 continue to own calibration behavior, export DTOs/builders, redaction, and off-thread export execution.
- `ui/mod.rs`, `app/{state,event,effect,reducer,snapshot}.rs`, and `app/mod.rs` are shared integration seams. Only one implementer may edit them at a time, and every task touching one must name exact paths in its commit.
- The existing resilience and diagnostics plans own backend algorithms, transport semantics, calibration math, registry truth, and export DTOs. These redesign plans add only shell projection, presentation, interactions, and integration tests around those contracts.

## Completion and Stop Gates

Subproject 1 is complete only when all ten plan tasks and the complete base-to-reviewed-HEAD commit range are approved, the package tests and release build pass, and the native executable is manually opened in German and English at both target sizes. At that point update the execution checkpoint, report the executable path and verification evidence, and stop.

Subproject 2 is complete only when every Required capability-matrix row outside diagnostics works end to end from widget to authoritative snapshot and no fake/disabled future controls remain.

Subproject 3 is complete only when support-safe export, diagnostics polling, accessibility, localization, DPI/theme, idle CPU, packaging, and hardware/manual acceptance evidence are recorded against one final integrated binary.

## Commit and Review Protocol

For every task:

1. Record the pre-task HEAD and exact dirty paths.
2. Run the task's focused test and observe its documented RED reason.
3. Implement only the task files with `apply_patch`.
4. Run focused tests, the affected crate suite, direct rustfmt checks for task-owned files, and `git diff --check`.
5. Stage exact task paths and inspect `git diff --cached --name-status` before committing.
6. Dispatch a fresh read-only reviewer against the complete base-to-HEAD task diff.
7. Fix and re-review all Critical/Important findings.
8. Update the execution ledger only at the subproject checkpoint, not by rewriting historical plans.

Do not combine tasks merely because they touch adjacent UI files. Their test and review gates are the reason they are separate.
