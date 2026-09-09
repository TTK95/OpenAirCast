# Clean integration — 2026-09-09

> Historical integration record, not an installation guide. Use the
> [README](../README.md) and [distribution guide](DISTRIBUTION.md) for portable
> download/build paths. Local artifacts below are not a GitHub publication.

The owner requested the GUI worktree be merged back and the repository cleaned
up, retaining the accepted Suspend/Cancel bug. The original product base was
`openaircast/main` at `8e04492`; the completed worktree head was `74c02e3`.
The main checkout fast-forwarded to that head without conflicts. No application
code was changed by integration or cleanup. `master` and unrelated branches
are not silently retargeted.

## Working locations at the time of integration

- Main checkout: the local repository root (where `Cargo.toml` resides).
- Product branch: `openaircast/main`.
- Optimized application staged at that time: `dist/OpenAirCast.exe`.
- Portable/source archives and checksums: see
  [RELEASE_ARTIFACTS_2026-09-09.md](RELEASE_ARTIFACTS_2026-09-09.md).

## Cleanup scope

Three tracked macOS metadata files (`.DS_Store`, `crates/.DS_Store` and
`crates/airplay-client/.DS_Store`) were removed. Existing ignore rules already
exclude them and local assistant/build artifacts. The clean source export
contains committed source only, without Git internals, worktrees, build caches
or local assistant state. No history rewrite or force push is part of cleanup.

Release files are copied into the main checkout and verified against their
checksums before retiring the completed GUI worktree. Its ignored work notes
are preserved privately under `.superpowers/archives/gui-worktree-notes-20260909.zip`;
previous root/worktree EXEs are backed up alongside that archive. Other worktrees
and unfinished branches remain untouched.

## Completion record

- Fresh tests from the merged main checkout: 2,104 passed, zero failures,
  15 ignored across 44 targets; process exit 0. No application code changed
  afterwards.
- Git unregistered the completed GUI worktree. Directory removal stopped at
  a Windows filename-length error; a scoped long-path cleanup attempt was
  blocked by the execution safety policy before running. The ignored remainder
  at `.worktrees/gui-control-center-implementation` therefore remains on disk;
  do not treat it as a working checkout or claim its cache space was reclaimed.
- Release artifacts and private notes/render evidence were preserved outside
  that directory before removal. The remaining source files are recoverable
  from Git. Unrelated worktrees were not removed.
- The clean source archive is exported from the committed main checkout,
  independent of ignored local leftovers. Its exact revision and hashes are
  recorded in the artifact manifest.

## Remaining limitation

Suspend during final activation followed by Cancel before resume settlement can
leave the UI at Stopping. The owner accepts this known issue; cleanup does not
fix it. No new native playback, standby or network test was run. The unsigned
release artifact is not a claim of complete 1.0, accessibility, license or
security acceptance. See [release notes](RELEASE_NOTES_2026-09-09.md).
