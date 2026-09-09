# Building, packaging and publishing OpenAirCast

## Current publication status

Checked on 2026-09-09: the public GitHub Releases list is empty. Prepared files
on a developer's disk are not a published release. The dated release/integration
records in this directory document earlier local checks, not an installation
location or a guarantee that their binaries match today's source.

The [README](../README.md) is the user-facing installation guide. All paths in
this document are relative to the repository root unless explicitly identified
as Windows user-data locations.

## Local build outputs

### Verified local optimized build, 2026-09-09

The corrected `build.ps1` was executed successfully: one embedded-asset test
passed, and the optimized build finished with exit 0 in 14 min 17 s. Existing
dependency warnings remain. The copied executable matches Cargo's output.
No app launch, playback test or full test-suite rerun was performed in this
distribution-only change.

- Application source: `07872d6863f0f671f332cb969b4fd4285dd08965`.
- Output: `dist/OpenAirCast.exe`, Windows x64, optimized, unsigned.
- Built at 2026-09-09 20:07:54 UTC; 24,068,096 bytes.
- SHA-256: `9AB6686A6F280C05FA81B4B7BBB69922874926EC3D2EE899F025CEB68E299A9A`.
- On the maintainer's checkout, the earlier portable directory, ZIPs and
  checksums were moved to `dist/archive/20260909-047dfc6/`, not deleted. The
  archived manifest's executable path was adjusted to its preserved subfolder;
  artifact hashes are unchanged. These local files are not included in a clone.

A new ZIP/source package has **not** been produced for this build. The archives
above belong to the older revision. The canonical local EXE is current, while
public-release availability remains as stated above.

### Rebuild on your own machine

Run `./build.ps1` on Windows with Rust/MSVC installed. It explicitly targets
`x86_64-pc-windows-msvc` and the repository's `target` directory, regardless of
the invoking terminal's Cargo output-directory configuration. It validates the
embedded assets, builds the optimized app and copies that output to
`dist/OpenAirCast.exe`. A failed test/build stops the script before that copy.
Quit a running copy from the tray before replacing it.

The script does not generate archives, sign the binary, upload a release or
refresh older ZIPs, checksum manifests or build records already in `dist`.
Regenerate publication metadata for each new build. `target` is a compiler cache/output
directory; `dist` is a local staging directory. Neither belongs in source Git.
Do not copy the debug executable into a release ZIP and label it optimized.

The script orchestration has a lightweight regression check:

```powershell
./tests/build-script.tests.ps1
```

It substitutes Cargo in an isolated fixture but executes the real build script's
error handling and file copying. It does not compile Rust or validate a binary.

## Release checklist (maintainer)

1. Choose and commit the release source revision/version. Run the application
   tests and optimized build from that revision. Record which Windows/hardware
   checks were actually performed. Keep the known Suspend/Cancel issue in the
   release notes until it is fixed and verified; signing and stability must not
   be implied by a successful build.
2. Stage a **new, versioned** portable directory such as
   `dist/OpenAirCast-<version>-windows-x64/`. Include `OpenAirCast.exe`, README,
   `LICENSE`, `THIRD_PARTY_NOTICES.md`, the referenced font/dependency license
   texts and the release's known-issues notes. Preserve their referenced paths.
   Do not silently reuse an old ZIP from an earlier source revision.
3. Create the portable ZIP and a matching source archive from the release tag.
   Inspect their contents and calculate SHA-256 checksums of the final archives.
   Exclude developer settings, assistant files, caches, logs and personal paths.
   An archive/checksum proves packaging identity, not working playback.
4. Create the matching tag and **GitHub Release**, then upload the portable ZIP,
   matching source archive and checksum file as **Assets**. This is the actual
   publication step, separate from `git push`. Use a pre-release designation
   while the documented development/validation limitations remain. This guide
   does not itself authorize an upload or changing an existing release.
5. Verify that the release and assets are publicly downloadable and that the
   downloaded hashes match. Only then replace the README's unavailable notice
   with the real release link. Update this status and the release evidence;
   never replace a public download link with a developer's `C:\Users\...` path.

## Installation, updates and removal

Users extract the portable package into a writable folder of their choice, for
example `%LOCALAPPDATA%\Programs\OpenAirCast`, and run `OpenAirCast.exe` there.
That folder is not created merely by cloning the Git repository.

User preferences and device/group state live in `%APPDATA%\OpenAirCast`, outside
the extracted app folder. Quit through the tray before updating the app files.
Removing the portable app folder removes the app; deleting its user-data folder
is a separate, deliberate reset of saved settings and groups.

There is no automatic updater, installer or package-manager installation
documented by this release workflow. Do not claim `cargo install` from the repo
is the supported distribution route; this workspace contains multiple crates
and uses the explicit `homepod-cast` / `openaircast` build shown in the README.
