# OpenAirCast Multi-room Implementation Notes

Date: 2026-08-22

## A. Where multi-room is implemented

The streaming group is not implemented by `DeviceGroup` alone. The relevant
code is distributed across:

- `airplay-client/src/client.rs`: group connect, shared live/file streaming,
  feedback, volume, stats, and retransmit dispatch.
- `airplay-client/src/connection.rs`: primary setup, `setup_for_group`,
  SETPEERS, timing state and per-session RTP sender construction.
- `airplay-audio/src/streamer.rs`: one decode/encode/schedule loop with N
  receiver targets.
- `airplay-timing/src/ptp.rs`: BMCA yield, slave offset calculation and Apple
  PTP TLVs.
- `airplay-client/examples/test_group.rs`: an older low-level experiment. It
  starts one decoder/streamer per connection and is not the optimal fan-out
  reference; the newer `AirPlayClient` group API is authoritative.

## B. PTP coordinator model

One primary `Connection` runs `run_bmca_yield_flow`. It advertises priority 250,
observes the HomePod's lower priority 248, yields mastership, and publishes the
HomePod clock identity plus continuously updated `ClockOffset` values.

Secondary connections call `setup_for_group(ptp_clock_id, timing_offset,
timing_rx)`. They do not create independent PTP coordinators because only one
process task can own the standard PTP ports. They mirror the primary watch
channel. This is the correct architecture for the implemented group: all media
is scheduled against one shared receiver clock rather than independently
estimated per receiver.

## C. Optimal fan-out point

PCM capture, timestamping, buffering, EQ and ALAC encoding are shared.
`streamer.rs` calls the encoder once per audio frame. It then calls
`prepare_audio()` on every `RtpSender` because each receiver has its own:

- SETUP-derived `shk` ChaCha20-Poly1305 key,
- RTP sequence/cipher nonce state,
- retransmission history,
- UDP data/control sockets and negotiated ports.

Therefore the encoded ALAC payload and media timestamp are the final shareable
artifacts. Encrypted wire bytes must remain per receiver.

## D. Windows-specific changes to preserve

- `crates/homepod-cast`: WASAPI loopback, native tray/settings UI, hotkey,
  keepalive and stop timeout.
- Cross-platform `socket2` send-buffer sizing in `airplay-audio/src/rtp.rs`.
- Unix-only DSCP/SO_PRIORITY guards.
- Optional AAC feature so the Windows ALAC app does not build FDK-AAC.
- ALAC encoder exports/configuration when AAC is disabled.

The AAC test gating bug is fixed separately; it does not justify reverting the
optional feature.

## E. Common playback epoch

The current upstream implementation completes SETUP for all receivers before
starting one `AudioStreamer`. That streamer waits for a shared initial buffer,
uses one RTP timestamp progression, applies the primary PTP offset, and sends
the same PT=87 sync content to all targets from one timed sender thread.

It does not expose a separate app-level `T = now + lead_time` barrier or a
per-receiver READY acknowledgement. The shared buffer start is the implemented
barrier and must be validated with physical receivers before stronger accuracy
claims are made.

## F. Dynamic joining

The source does not contain a working dynamic join path for an active stream.
`add_to_group()` only mutates `DeviceGroup` metadata and sends SETPEERS; it does
not create/setup a new `Connection`, add an `RtpSender`, send the correct marker
packet, or align the new receiver's buffer with the current epoch.

For the first Windows MVP, a membership change stops and renegotiates the group.
This is an implementation limitation. Determining whether a seamless protocol
join is interoperable requires a dedicated source/packet-capture study and real
HomePod validation.

## Failure isolation already present

- ALAC is produced once; a failed UDP `send_to` is logged per target and does
  not terminate sends to later targets.
- Retransmit requests and histories are per receiver.
- Secondary feedback errors are ignored so they do not fail the primary.

## Failure isolation still missing

- Partial-success group setup.
- Receiver state machine wired to actual setup phases.
- Detection of a silent/disappeared receiver.
- Backoff, rediscovery, re-pair and synchronized rejoin.
- Deterministic shutdown of every auxiliary control thread.

