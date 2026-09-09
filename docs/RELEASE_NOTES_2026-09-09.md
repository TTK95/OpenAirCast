# OpenAirCast 0.1.0 — Windows x64, 2026-09-09

## Release decision

The owner explicitly requested commit, merge and push of the current state,
accepting the known Suspend/Cancel defect, and preparation for release.
This decision supersedes the earlier integration hold for that defect. It
does not turn an unperformed test into a pass. No defect fix is included in
this release-preparation step; application source is `047dfc6070cf42d8404aca779dd17c57e5af6358`.

## Included

- Compact Windows Native control center with shared speaker selection.
- Independent receiver levels and restored confirmed master volume.
- Safe initial level application at the actual audio-release boundary.
- Real Windows loopback playback-source enumeration and failure feedback.
- Saved groups with an isolated editor, explicit apply/start/delete and retained
  offline members and levels.

## Known accepted issue

**Suspend during stream activation, followed by Cancel before the two-second
wake-settle interval ends, can leave the UI stuck at Stopping.** The prepared
session was torn down but its Stopped acknowledgement is missing.

Avoid suspending Windows while a start is still in progress. If affected,
explicitly Quit from the tray and relaunch; this workaround is not newly
hardware-validated. Merely closing the window minimizes to the tray and does
not exit the application. The next code change must cover this exact sequence
with a failing regression, preserve the prepared generation's Stopped result
after bounded cleanup, and receive independent review.

## Evidence and limits

The preceding source verification passed 2,104 tests with 0 failures and 15
ignored cases, including real local-loopback RTP gate tests. Clippy completed
with existing warnings. Fresh integration/release build evidence and artifact
hashes are recorded in [the verification record](RELEASE_CHECK_2026-09-09.md).

No new playback, standby, network interruption or native app launch is part of
this preparation. Native screenshot capture is blocked by Windows
`SetIsBorderRequired`/`0x80004002`; visual/navigation/accessibility acceptance is
not established. Thirty-minute listening, measured skew, physical endpoint
switching/rejoin, Windows 10/11 normal-user/DPI/NVDA and clean-machine checks
remain unperformed. Packaging does not constitute a full dependency-license or
security audit. The portable build is unsigned; no signing or stable-1.0
quality certification is claimed.

## Distribution

Version remains 0.1.0; this preparation does not invent a new version or tag.
An optimized Windows x64 portable build and local archive are prepared with
the existing project/font license texts and third-party notices. Matching
source is retained in the repository. Preparing these files and pushing code
does not itself publish a GitHub Release or upload binary assets.
