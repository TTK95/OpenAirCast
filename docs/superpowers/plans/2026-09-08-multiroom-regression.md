# Multiroom Regression — Execution Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development and superpowers:test-driven-development. Implement only the concrete task below; protocol fixes require measured evidence and a subsequent task.

**Goal:** Reproduce the reported two-receiver silence without capturing personal system audio, then fix the evidenced fault.

**Architecture:** Exercise the same `AirPlayClient::new` / `connect_group_best_effort` construction used by the shipping backend. A setup-only `--diagnose-group` CLI route in the actual OpenAirCast binary isolates discovery, pairing and PTP from WASAPI and RTP. It neither changes volume nor starts audio. Keeping the real binary matters because Windows firewall rules are executable-path-specific.

**Tech Stack:** Existing Rust workspace, Tokio, tracing, no new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-08-openaircast-complete-product-design.md`, P6; latest user evidence: both selected speakers silent.

## Global Constraints

- One capture/encode pipeline; no parallel hardware sessions.
- No speculative timing constants, no silent downgrade of authentication.
- No user settings, identities, firewall rules or running user processes modified by the diagnostic.
- Setup success is not audible playback success. No automatic hardware pass.
- Keep changes in the existing linked worktree. No push or release as a side effect.

### Task 1: Explicit two-receiver setup diagnostic

**Files:** Create `crates/homepod-cast/src/setup_diagnostic.rs` (including its tests); modify `crates/homepod-cast/src/main.rs` only for module declaration, logging selection and the new CLI route.

**Interfaces:** Consume existing `AirPlayClient::{new,discover,connect_group_best_effort,disconnect}`. No new public library API. Add `pub async fn run(args: Vec<String>) -> anyhow::Result<()>` in the private app module. Produce `openaircast --diagnose-group <exact-name-1> <exact-name-2>`. Require this flag at argument 1 and exactly two distinct names afterwards; never fall through to GUI/selftest for malformed diagnosis arguments. Do not default to the first devices found.

- [x] RED: add pure argument/selection tests first. Validate exactly two nonempty distinct names, missing device, ambiguous name, and reversed input ordering. Reuse a helper operating on discovered names/indices so fake protocol devices are unnecessary. Representative assertions:

```rust
assert!(requested_names(vec![]).is_err());
assert!(requested_names(vec!["Büro".into(), "Büro".into()]).is_err());
let names = requested_names(vec!["Büro".into(), "Bad".into()]).unwrap();
assert_eq!(select_indices(&names, &["Bad", "Büro"]), Ok([1, 0]));
assert!(select_indices(&names, &["Büro", "Büro", "Bad"]).is_err());
assert!(select_indices(&names, &["Bad"]).is_err());
```

- [x] Run `cargo test --offline -p homepod-cast --bin openaircast setup_diagnostic --quiet`; record the intended failure before implementing helpers.
- [x] GREEN: implement the pure helpers with these signatures:

```rust
fn requested_names(args: Vec<String>) -> Result<[String; 2], String>
fn select_indices(requested: &[String; 2], discovered: &[&str]) -> Result<[usize; 2], String>
```

  Require exact names (reject empty/whitespace-only, retain nonempty spelling). Locate exactly one index per requested name; reject duplicates/missing names before connecting.
- [x] Implement bounded CLI orchestration: parse args before networking; create client; discover for 3 seconds; select exactly those devices; reject password-required or non-transient-capable devices. Log only explicit diagnostic summary and filter `off,airplay_timing::ptp=debug,airplay_client::connection=info` (not crypto/pairing raw output); diagnostic mode ignores a more verbose RUST_LOG override. Print that this sends SETUP/RECORD and attempts TEARDOWN but sends no audio and changes no volume. It must run without another session. Use a 30-second group-setup deadline and always attempt the public disconnect API under a 5-second deadline after any setup result. Observe accepted setup for 3 seconds before disconnect to collect timing logs. On error/timeout, connected membership is unknown, not zero. Public disconnect returning Ok is only `disconnect_api=returned_ok`, NOT proof all member TEARDOWNs were acknowledged: existing library swallows secondary errors and cancellation can drop pending links before they enter client storage. Always print that per-member teardown and audio remain unverified; this initial inspection-only tool must return nonzero even after accepted setup until library cleanup observability is fixed. No SUCCESS/complete/clean shutdown claims. Capture actual setup counts/phases when available and no raw errors/identity objects in CLI summary. No `start_live`, source capture, playback, volume or settings calls. Main owns Tokio runtime; after run returns, use the existing CLI convention `shutdown_background()` so detached workers cannot indefinitely hold this inspection-only process open. Do not introduce a shutdown deadline into the normal app path. No runtime/GUI startup for invalid diagnostic args. Do not alter old normal/selftest behavior. Add focused outcome tests proving an accepted setup plus Ok disconnect cannot be classified as verified success, and cancelled/failed setup never claims zero connected receivers.
- [x] Run focused binary tests and `cargo build --offline -p homepod-cast --bin openaircast`; format only owned files (rustfmt main with skip_children to avoid touching unrelated modules). Do not execute real hardware from the implementer.
- [x] Independent review, then root runs exactly one setup probe after the app is closed. Use the user's named pair when provided; otherwise explicitly announce a pair of the already discovered HomePods and mark it as a diagnostic sample, not an exact reproduction of an unspecified pair. Store any raw local logs only under ignored `target`; document redacted findings.

### Task 2: Confirm the observed clock-domain transition

**Files:** `crates/airplay-timing/src/ptp.rs`, only `run_bmca_yield_flow` phase-4 general-socket branch. No protocol behavior or field changes.

**Evidence:** First hardware sample accepted both members. The initial valid offset changed by approximately 1,007,269 seconds immediately after the second member joined. The exported clock identity is currently fixed once. Confirm an actual Announce grandmaster change before designing the fix.

**Interface:** Existing PTP debug logging; no public API changes. Add this observation inside the existing successful header-parse branch in the phase-4 general-socket receive path, immediately before the DelayResp branch:

```rust
if header.message_type == PtpMessageType::Announce && len >= 61 {
    tracing::debug!(
        "BMCA slave: Announce grandmaster={:02x?}, initial={:02x?}",
        &general_buf[53..61], remote_clock_id
    );
}
```

- [x] Add only the above trace. This is temporary diagnostic instrumentation, not a timing fix; RED/GREEN functional regression belongs to the following evidence-based fix task.
- [x] Run `cargo test --offline -p airplay-timing --lib --quiet` (baseline 81 tests); no broad formatting of this pre-existing file.
- [x] Independent scoped review verifies packet length guard and phase-4 insertion, no behavior change.
- [x] Root builds real app, verifies previous process/ports ended, then performs one setup-only sample to confirm/rule out changing grandmaster. No audio, no settings/firewall changes.

### Task 3: Propagate coherent PTP clock generations

**Files:** `crates/airplay-timing/src/{ptp.rs,lib.rs}`, `crates/airplay-client/src/{connection.rs,group.rs,client.rs}`, `crates/airplay-audio/src/streamer.rs`, focused existing client group tests as required. No UI, capture, latency, authentication or lifecycle refactor.

**Evidence:** Second hardware sample confirms Announce grandmaster A becomes B during group formation. Offset tracks B but the one-shot identity remains A. The functional fix must remove this torn-clock state and permit a backward as well as forward epoch change.

**Interfaces and behavior:**

```rust
#[derive(Debug, Clone, Copy)]
pub struct PtpClockState {
    pub master_clock_id: [u8; 8],
    pub offset: ClockOffset,
}
// Authoritative live PTP state; None = not measured / transitioning.
// watch::Receiver<Option<PtpClockState>>
```

- Add a state-based BMCA entry point while retaining the existing `run_bmca_yield_flow` signature as a compatibility adapter. Publish with `send_replace` so late subscribers see the latest state. Production connection uses only the coherent state path for PTP.
- Extract a pure packet measurement tracker, invoked by the actual socket loop. Validate lengths before reading fields. Track announced GM identity separately from the relay's source-port identity. On Announce identity/source change clear pending Sync/Follow_Up/Delay state and publish None. Pair Sync/Follow_Up by sequence, source-port identity and current generation, accepting packets only from the announced relay and existing target IP filter. A matched sample publishes one complete state. Repeated identical Announces must not invalidate a good sample. No zero-ID/default-offset ready state. Any retained Delay_Resp update must validate request sequence and requesting-port identity and stay in its generation; it must not bypass tracker coherence.
- `Connection` owns/subscribes to the state channel. Wait boundedly for the first measured Some value instead of sleeping 500 ms and accepting a default offset. Preserve legacy public APIs where practical as adapters/snapshot views, but production PTP consumers must not combine independent ID/offset reads. Add `ptp_clock_rx()`. Keep NTP behavior unchanged.
- Primary and secondaries share the same state channel (no secondary forwarding worker for this new path). `GroupPrimaryTiming` carries the shared state receiver. Update fake test constructors consistently. A legacy `setup_for_group` adapter may remain, but the real group path uses a coherent receiver.
- Configure every production BMCA-backed file/live/group streamer via `set_ptp_clock_updates(...)`. Existing legacy offset/NTP setters may remain, but must not overwrite live coherent PTP state.
- Under the streamer's single state lock, consume the latest complete clock state before preparing packets. None suppresses PT87, never sends a default or mixed-domain PT87. On GM change install ID+offset together, reset the old epoch's `last_master_anchor_ns` and sync cadence, and force the next PT87 for every target. One encode/RTP timestamp pipeline stays intact; do not change render lead or calibration.

- [x] RED: failing tracker tests for A Sync → Announce B → stale A Follow_Up (no publication), matched B sample (only B/new offset), unchanged relay identity across the transition, mismatched sequence/source, repeated Announce, late subscribers, invalid/zero initial identity. Include bounded initial-readiness failure.
- [x] RED: streamer regression tests use actual state consumption/frame preparation, not a disconnected replica. A/old → None → B/new never produces A/new or B/old, None suppresses PT87, backwards epoch resets old anchor and forces sync. Check at least two targets preserve common RTP timestamp and coherent clock pair.
- [x] GREEN: minimal implementation above; add client integration coverage that group links/streamer receive the same coherent state and live transition, preserving legacy and NTP paths.
- [x] Run focused regression tests (record intended RED failures), then `cargo test --offline -p airplay-timing -p airplay-audio -p airplay-client` and app binary tests/build. Format owned code only; do not mechanically rewrite unrelated legacy areas.
- [x] Independent spec and quality review, fix findings, rerun relevant tests. Root may repeat setup-only inspection afterward; audible playback remains a separate human-observed gate.

**Scope boundary:** This addresses observed clock-domain transitions, not complete BMCA elections, precision peer delay, packet-loss recovery or lifecycle cancellation. Cross-socket late traffic cannot be perfectly dated without additional protocol information; document any residual association limit rather than claiming arbitrary network-reordering correctness.

**Implementation checkpoint:** Timing producer and all four production BMCA file/live/group consumers are migrated and independently reviewed (both slices: spec and quality PASS). Exported entry point is `run_bmca_yield_flow_state`. The primary also projects complete offsets for legacy callers within its existing worker; production uses the coherent channel, and production secondaries add no forwarding worker.

**Verification, 2026-09-08:** `cargo test --offline --workspace --quiet` exited0: 1,997 passed,0 failed,13 ignored including doctests. Focused timing/audio/client run: 441 passed,0 failed,7 ignored. Normal `openaircast` debug binary build and legacy `test_group` example check passed. Existing compiler warnings remain; this is not a warning-free release certification. `git diff --check` passed.

**Current-build setup-only sample:** Two HomePod minis accepted SETUP/RECORD; zero reported setup failures. Two clock-transition notifications and 37 coherent measurements spanning two grandmasters were recorded, including 26 measurements after the last transition. App process and PTP port bindings ended; settings/state hashes remained unchanged. The probe intentionally exits1 because per-member teardown and audio are not independently verified. No audio or volume change, no firewall modification. Raw local identifiers remain only in ignored logs.

**Remaining limitations:** Audible playback must be confirmed by the owner before closing the original both-silent report. The legacy offset-only API cannot express identity changes or invalidation. A very rapid same-ID A→None→A transition can be coalesced by `watch`; explicit generation counters may be needed if same-ID clock resets become a measured problem. Arbitrary cross-socket packet ordering, complete BMCA/Pdelay and cancellation/secondary teardown guarantees remain separate reliability work.

**Lint checkpoint:** Scoped timing/audio/client `cargo clippy --all-targets` exits0 with existing warnings. The moved tracker absolute-value cast warning was removed by an independently reviewed equivalent `unsigned_abs` substitution. A strict cast-lint probe still reports four pre-existing occurrences elsewhere; the repository is not warning-free. The setup-only hardware sample precedes this equivalent lint-only substitution; final offline verification/build follows it.

## Next decision

**2026-09-08 measured result:** Both setup-only samples accepted two HomePod minis. The second sample confirms that the primary's phase-4 Announce grandmaster changes after the secondary joins. The offset changes by about 11.66 days, but the exported master identity remains the initial one. This establishes a stale-clock-identity defect; it does not yet establish audible recovery. Both inspection processes exited and left no app process or bound PTP ports. User settings and firewall rules were not changed. Raw device identifiers/logs remain only in ignored local `target/` files.

Independent review found pre-existing cleanup gaps in `connect_group_best_effort` cancellation and `AirPlayClient::disconnect` secondary-error handling. This read-only-in-audio probe must expose the uncertainty instead of expanding into an unplanned library lifecycle refactor. Add those gaps to the P6 reliability backlog, separate from the silence cause. A failed or timed-out hardware probe is not immediately retried: verify process/ports ended and report any remaining uncertainty.

Add the smallest regression-test/fix task supported by the probe. A default/invalid PTP identity accepted as ready is an observed code risk, not yet the proven cause. The alternate config hypothesis is weaker: the audio streamer does not read the differing `timing_protocol`/ASC/latency fields and explicitly receives PTP mode. Do not patch it without behavioral evidence.

## Progress

- [x] User approved implementation and clarified both receivers are silent.
- [x] Discovery-only probe found three HomePods; no audio sent.
- [x] Independent cheap read-only agent compared shipping/legacy paths.
- [x] Setup-only diagnostic and regression evidence (671 binary tests passed, 2 ignored; 81 timing tests passed; independently reviewed).
- [x] Evidenced fix and offline regression tests (independently reviewed and built).
- [x] Current audible playback confirmation: the owner tested all three speakers
  and confirmed simultaneous playback on 2026-09-08. No duration or quantitative
  synchronization measurement was supplied.
- [ ] Resume P1 UI tasks.
