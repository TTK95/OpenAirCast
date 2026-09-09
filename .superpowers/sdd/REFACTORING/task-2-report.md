# Task 2 report — separate large test bodies without widening APIs

## Scope completed

Moved the five existing private inline test-module bodies into standard private
child-module files:

- `ui/pages/mod.rs::destination_tests` -> `ui/pages/destination_tests.rs`
- `ui/presentation.rs::tests` -> `ui/presentation/tests.rs`
- `ui/presentation.rs::command_home_tests` -> `ui/presentation/command_home_tests.rs`
- `app/reducer.rs::receiver_level_tests` -> `app/reducer/receiver_level_tests.rs`
- `app/reducer.rs::tests` -> `app/reducer/tests.rs`

Each production parent retains `#[cfg(test)] mod <original_name>;`. No module was
made public and no production behavior was edited.

The source-inspection guards moved with their tests. Paths that became relative
to a new child directory were intentionally changed to
`include_str!("../presentation.rs")` and `include_str!("../reducer.rs")`.
The page test file remains adjacent to the page renderer files, so its existing
`include_str!("overview.rs")`, etc. paths remain correct.

## Commands and observed output

Every Cargo process ran at `BelowNormal`, one at a time, with target
`x86_64-pc-windows-msvc`, shared target directory
`C:/Users/Thorsten/Documents/Claude/Projects/HomePodCast/target`, `-j2`, and
`--test-threads=2`.

Pre-refactor discovery:

```text
cargo test -p homepod-cast --bin openaircast --target x86_64-pc-windows-msvc --target-dir C:/Users/Thorsten/Documents/Claude/Projects/HomePodCast/target -j2 -- --list --test-threads=2
786 tests, 0 benchmarks
```

Focused suites after extraction:

```text
cargo test ... ui::pages::destination_tests -- --test-threads=2
test result: ok. 86 passed; 0 failed; 0 ignored; 700 filtered out

cargo test ... ui::presentation:: -- --test-threads=2
test result: ok. 77 passed; 0 failed; 0 ignored; 709 filtered out

cargo test ... app::reducer:: -- --test-threads=2
test result: ok. 83 passed; 0 failed; 0 ignored; 703 filtered out
```

Post-refactor discovery:

```text
cargo test -p homepod-cast --bin openaircast --target x86_64-pc-windows-msvc --target-dir C:/Users/Thorsten/Documents/Claude/Projects/HomePodCast/target -j2 -- --list --test-threads=2
786 tests, 0 benchmarks
```

The pre/post name sets were parsed from the two fresh listings and compared:

```text
before=786
after=786
added=[]
removed=[]
```

Final package acceptance matrix:

```text
cargo test -p homepod-cast --target x86_64-pc-windows-msvc --target-dir C:/Users/Thorsten/Documents/Claude/Projects/HomePodCast/target -j2 -- --test-threads=2

lib unit tests:                   163 passed, 0 failed, 0 ignored
openaircast binary unit tests:    780 passed, 0 failed, 6 ignored
backend_lifecycle:                 36 passed, 0 failed
calibration:                       18 passed, 0 failed
device_resilience:                 12 passed, 0 failed
diagnostics_contract:              13 passed, 0 failed
diagnostics_export:                24 passed, 0 failed
persistence:                       10 passed, 0 failed
recovery:                           8 passed, 0 failed
session_recovery:                  19 passed, 0 failed
system_recovery:                   35 passed, 0 failed
doc tests:                          0 passed, 0 failed
TOTAL:                           1118 passed, 0 failed, 6 ignored
```

Formatting and diff validation:

```text
rustfmt --edition 2021 <three parents and five extracted test files>
git diff --check
# no output
```

## Self-review

- Exact module names and nesting are preserved, confirmed by the identical
  786-name discovery sets.
- All declarations remain private and binary-local; no integration tests or
  public API changes were introduced.
- Assertions and production behavior were not weakened or changed.
- Source guards still inspect the intended production files from their new
  filesystem locations.
- The only warnings in the Cargo output are pre-existing warnings from
  dependency crates (unused/deprecated/dead-code warnings); Task 2 introduced
  no new warning or failure.

## Concerns

None specific to this refactor. The six ignored tests are the same platform or
manual-visual tests present in the baseline acceptance matrix.
