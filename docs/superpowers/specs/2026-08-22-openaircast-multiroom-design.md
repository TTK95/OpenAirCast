# OpenAirCast Multi-room MVP Design

## Scope

Deliver the smallest Windows path that lets a user select at least two
discovered AirPlay 2 receivers, captures the default Windows render stream once,
and uses the existing upstream PTP/multi-target streamer to send synchronized
ALAC audio to the selected group.

This milestone includes honest group state, a multi-select tray and regression
tests for protocol-independent membership behavior. It does not claim dynamic
join, reconnect, 30-minute synchronization, or physical HomePod validation.

## Architecture

The UI owns only a pure selected-group model and sends complete desired member
sets to a control thread. The control thread owns Tokio and one `cast::Session`.
`Session` owns one WASAPI capture thread plus either the existing single
`Connection` or one `AirPlayClient` containing the primary and secondary group
connections.

For a group, all receivers finish pairing/SETUP/PTP before the live decoder is
started. WASAPI pushes frames into one bounded `LiveFrameSender`. The shared
upstream streamer decodes/buffers/encodes once and performs receiver-specific
RTP encryption and transmission.

## Membership behavior

- Each receiver tray item is independently checked.
- Checking or unchecking a receiver updates the desired group.
- Empty selection means stopped.
- The global hotkey stops the active group and resumes the last non-empty group.
- Because upstream has no working live sender insertion API, any membership
  change renegotiates the group and is logged as such.
- A failed group start returns the tray to stopped while preserving the desired
  group as the hotkey retry target.

## Error handling

Startup errors are returned with receiver/group context. Capture startup failure
stops the capture thread and tears down already-created AirPlay sessions.
Shutdown is bounded by the existing timeout. Runtime sender errors remain
structured log events; they are not converted into invented sync metrics.

## Testing

- Feature-disabled AAC tests compile only when the AAC feature exists.
- A pure `GroupSelection` unit test suite covers add, remove, stop/resume,
  duplicate toggles and stale-device filtering.
- `cargo test --workspace --all-targets` proves the non-hardware paths compile
  and run.
- `cargo build -p homepod-cast --release` proves the no-console Windows binary
  can be produced.
- Physical validation remains a named blocker: two HomePods, 30 minutes,
  disconnect/rejoin and repeated start/stop.

