# Prepared release artifacts — 2026-09-09

OpenAirCast 0.1.0, Windows x64. These are local artifacts, not an uploaded
GitHub Release. The owner accepts the known Suspend/Cancel issue described in
[release notes](RELEASE_NOTES_2026-09-09.md). Unsigned; native/physical and full
license/security acceptance remain unperformed, not implicitly passed.

| Artifact under `dist/` | SHA-256 |
|---|---|
| `OpenAirCast.exe` | `7F155CD26E49C61694C2B04DD1C3ED37E6677D2AA60C5064B8D262E35533C599` |
| `OpenAirCast-0.1.0-windows-x64-20260909.zip` | `349B73381FD14FBFE5FF972FEF1342B39DC91269415EB66A2249833BD888BF05` |
| `OpenAirCast-0.1.0-source-20260909.zip` | `6598F368147DCC786F15F7E11E7DC213210B08A04F148543F4D092BFE0FDBCD5` |

The optimized EXE is 23,602,688 bytes, PE AMD64, Windows GUI subsystem.
The application source is `047dfc6070cf42d8404aca779dd17c57e5af6358`;
the matching source ZIP is `git archive` of
`1288dce9cda919522eec1b01f7eeba5e5b67adf4` on `openaircast/main` (cleaned
metadata and integration documentation, identical application source).
This manifest is committed after archive creation to avoid a self-referential
checksum. Its copy inside the source snapshot predates this refreshed packaging
record; use the checksums above for the current archives. All 298 source archive
entries were checked: no `.DS_Store`, Git internals, assistant state, worktrees
or build caches. The portable package includes the post-merge verification record.
No submodules or local build/cache files are required by that source snapshot.
Dependencies are identified by Cargo.lock; building requires the documented
Rust/MSVC toolchain and dependency cache or an initial dependency fetch.

The binary ZIP was opened and its eight entries inspected: EXE, README,
project LICENSE, third-party notices, release notes/check record and both
font license texts. Hashing the EXE directly inside the ZIP matched the build
output and canonical `dist/OpenAirCast.exe`. This checks packaging integrity,
not normal-user clean-machine execution or a comprehensive license audit.

Fresh verification: 2,104 passed, 0 failed, 15 ignored across 44 targets;
optimized build exit 0. See [full evidence](RELEASE_CHECK_2026-09-09.md).
Previous executable and pre-cleanup archive versions are recoverably preserved
privately under `.superpowers/archives/` in the main checkout; they are not the
current release artifacts. All current files above are in the main checkout's
`dist/`, not the retired GUI worktree. See the
[integration record](INTEGRATION_2026-09-09.md) for the remaining ignored folder
whose deletion was blocked; it is excluded from these clean exports.
