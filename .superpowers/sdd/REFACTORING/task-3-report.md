# Task 3 report: split the backend bridge

## Scope and result

- Base: `62939c99a1e46e514ba84ff225c2d737ce864bc6` on `codex/source-refactor`.
- Replaced `apps/openaircast/src/backend_bridge.rs` with the same
  `backend_bridge` module name implemented by:
  - `mod.rs`: module documentation and the compatibility facade;
  - `commands.rs`: command/effect translation and request correlation;
  - `projection.rs`: pure snapshot/event projection and opaque endpoint keys;
  - `runtime.rs`: bridge worker, bounded outbox repair, diagnostics polling,
    shutdown, and backend wiring;
  - private `tests.rs`: the original contract tests.
- No dependencies, feature flags, protocols, queue sizes, time budgets,
  ownership, thread model, or behavior changed. No extra helper file was needed.

The split was a documented mechanical bulk rewrite: fixed source ranges were
copied from the original file into the responsibility files, then imports and
the minimum sibling visibility were made explicit. `rustfmt --edition 2021`
was used directly on the five files because the task requires every Cargo call
to carry an explicit target, while `cargo fmt` has no target option.

## Logic-preservation review

A token-normalized comparison was run between the three extracted production
bodies and their exact ranges in `HEAD:apps/openaircast/src/backend_bridge.rs`.
The comparison removed whitespace, the new `pub(super)` qualifiers, and six
rustfmt-required trailing parameter commas introduced when those qualifiers
made formerly one-line signatures wrap.
Results:

```text
commands_logic_equal=True
projection_logic_equal=True
runtime_logic_equal=True
```

This covers the generation/revision guards, group receipt and backend-attempt
correlation, stale/unknown projection states, bounded retained outbox,
shutdown budgets, and the current-thread runtime loop. The extracted runtime
still has one owner of mutable backend state and holds no lock across an
`await`.

One off-by-one extraction artifact temporarily placed the original test-only
attribute before `Projection`; the exact-body audit caught it, it was removed,
and all final verification below ran after the correction.

## Visibility and facade comparison

The original crate-visible item set was:

```text
backend_config_for
backend_device_seam
BackendPort
BridgeHandle
DEVICE_SHUTDOWN_BUDGET
diagnostics_reading
EndpointDirectory
EndpointTable
GenerationEcho
GenerationsSeen
legacy_volume_source
project_event
Projection
SessionEdge
spawn_backend_bridge
```

`backend_bridge/mod.rs` re-exports exactly this same set with `pub(crate)`.
The child modules are private. Former single-module private members needed by
a sibling or by the private root tests use `pub(super)` only; nothing became
public outside the existing `backend_bridge` scope. An `unused_imports`
allowance is attached only to the compatibility re-export statements because
several old crate-visible names are contract-test seams and are intentionally
unused by the release binary.

## Structural boundary guard

The existing recursive source scan previously exempted the one
`backend_bridge.rs` file. It now:

1. asserts that `src/backend_bridge` exists as a directory;
2. asserts its inventory is exactly `commands.rs`, `mod.rs`, `projection.rs`,
   `runtime.rs`, and `tests.rs` (no unchecked extra file or subdirectory);
3. exempts only that exact verified module subtree while continuing to scan
   every other Rust source for direct `homepod_cast::` access.

This changes the guard for the new layout without widening its allowlist.

## Verification commands and results

All Cargo processes were started at Windows `BelowNormal` priority, serialized
(one Cargo process at a time), with two build jobs, the explicit Windows MSVC
target, and the shared target directory.

Focused backend bridge:

```text
cargo test --locked -p homepod-cast --bin openaircast \
  --target x86_64-pc-windows-msvc \
  --target-dir C:/Users/Thorsten/Documents/Claude/Projects/HomePodCast/target \
  -j2 backend_bridge:: -- --test-threads=2

135 passed; 0 failed; 0 ignored; 651 filtered out
```

Final package gate (also the required UI acceptance gate):

```text
cargo test --locked -p homepod-cast \
  --target x86_64-pc-windows-msvc \
  --target-dir C:/Users/Thorsten/Documents/Claude/Projects/HomePodCast/target \
  -j2 -- --test-threads=2

library: 163 passed; 0 failed; 0 ignored
main binary: 780 passed; 0 failed; 6 ignored
backend_lifecycle: 36 passed
calibration: 18 passed
device_resilience: 12 passed
diagnostics_contract: 13 passed
diagnostics_export: 24 passed
persistence: 10 passed
recovery: 8 passed
session_recovery: 19 passed
system_recovery: 35 passed
doc tests: 0
package total: 1118 passed; 0 failed; 6 ignored
```

The library subtotal was also re-run directly from the shared cache with the
same constraints (`cargo test ... -p homepod-cast --lib`): 163 passed, 0
failed, 0 ignored.

The main-binary run included all 41 tests whose names start with
`ui::acceptance::`; all passed. Their groups were accessibility, compact
volume/layout, controls, copy, coverage, events, groups, preferences,
redaction, release-group lifecycle, visual-review renders, declaration scan,
and event-sink behavior.

Final non-test compile check after the compatibility-facade lint annotation:

```text
cargo check --locked -p homepod-cast --bin openaircast \
  --target x86_64-pc-windows-msvc \
  --target-dir C:/Users/Thorsten/Documents/Claude/Projects/HomePodCast/target \
  -j2

exit 0; no warning from apps/openaircast or the split files
```

The commands continued to report pre-existing warnings in dependency crates;
no warning originated in the changed app files. `git diff --cached --check`
also completed with no errors.

The supplied baseline (`1118 passed / 6 ignored` on the base commit) was not
rerun, per the task instruction to avoid a duplicate baseline suite.
