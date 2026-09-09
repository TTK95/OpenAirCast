# OpenAirCast Architecture Analysis

Date: 2026-08-22

## Executive summary

`iakacer/windows-airplay-homepod` is not behind `lmcgartland/airplay2-rs`. The
current HomePod Cast commit `cc9a2fe` is three Windows-specific commits ahead of
the current `airplay2-rs` commit `a7f019f`, and `a7f019f` is their exact merge
base. The upstream multi-speaker/PTP implementation is already present in the
checkout. The missing piece is application integration: HomePod Cast directly
owns one low-level `Connection`, configures it for NTP, and exposes a
single-selection tray menu.

The smallest safe integration is to keep the Windows capture code and use the
existing `AirPlayClient::connect_group()` and
`AirPlayClient::start_live_streaming_to_group()` path. That path creates one
`LiveAudioDecoder`, one buffer, one ALAC encoder, and one scheduler, then fans
out at the last protocol-valid boundary to receiver-specific RTP senders.

## Git relationship

| Repository/ref | Commit | Relationship |
|---|---|---|
| `airplay2-rs/master` | `a7f019f` | Exact merge base and current protocol upstream |
| `homepod-cast/master` | `cc9a2fe` | Three commits ahead of `a7f019f` |

HomePod Cast additions after the merge base:

1. `b0234c3` — Windows tray app, WASAPI loopback, Windows RTP portability,
   optional AAC feature.
2. `28151cb` — AirPlay feedback keepalive and volume control.
3. `cc9a2fe` — native settings window and configurable hotkey.

There are no upstream commits missing from HomePod Cast at the time of this
analysis. The divergence is additive and Windows-specific. It must be preserved
instead of replaced by a fresh upstream import.

## Workspace/crate map

| Crate | Responsibility | Relevance to OpenAirCast |
|---|---|---|
| `airplay-core` | Device, capability, stream and error types | Stable shared model |
| `airplay-discovery` | mDNS/DNS-SD browser and TXT parsing | Keep; continue discovery while streaming in a later milestone |
| `airplay-crypto` | HKDF, SRP, Curve25519, Ed25519, ChaCha/AES | Keep unchanged |
| `airplay-pairing` | Transient and persistent HomeKit pairing | Keep unchanged |
| `airplay-rtsp` | Encrypted RTSP, plist SETUP/SETPEERS/session state | Keep unchanged |
| `airplay-timing` | NTP, PTP, BMCA yield and group-master experiments | Reuse BMCA path; do not duplicate in app |
| `airplay-audio` | Decode/live input, ALAC, buffering, RTP, multi-target scheduler | Reuse shared streamer |
| `airplay-client` | High-level connection and multi-speaker orchestration | Primary integration boundary |
| `homepod-cast` | WASAPI loopback, tray, settings, hotkey | Evolve into OpenAirCast app layer |

## Current Windows application architecture

```text
tray/main thread
    -> std::mpsc command
control thread + Tokio runtime
    -> cast::Session (exactly one Connection)
    -> WASAPI capture thread
    -> LiveAudioDecoder
    -> Connection::start_streaming_live
    -> one AudioStreamer / one RtpSender
```

`cast::Session::start` hard-codes `TimingProtocol::Ntp`. `tray.rs` stores the
active receiver as `Option<usize>` and stops the old session before starting a
different receiver. Discovery is a one-shot scan before the tray menu is built.

## Existing upstream multi-room architecture

The working high-level entry points are in
`crates/airplay-client/src/client.rs`:

- `AirPlayClient::connect_group(&[Device])`
- `AirPlayClient::start_live_streaming_to_group(LiveAudioDecoder)`
- `AirPlayClient::send_feedback()`
- `AirPlayClient::set_volume()`
- `AirPlayClient::stats_snapshot()`

The setup sequence is:

1. Pair/connect every selected receiver.
2. Force PTP in the per-connection stream configuration.
3. Run normal `Connection::setup()` for the primary receiver.
4. The primary executes the Mac-style BMCA yield flow: sender priority 250,
   HomePod priority 248, then the sender becomes a PTP slave.
5. Obtain the primary HomePod clock identity and a watch channel containing
   clock-offset updates.
6. Call `setup_for_group()` for each secondary. Secondaries do not bind another
   PTP coordinator; they receive the primary timing state.
7. Send the complete SETPEERS address list to each session.
8. Build one `AudioStreamer` with N receiver-specific `RtpSender` values.
9. Fill the shared live buffer before the sender thread begins transmitting.

`DeviceGroup` in `crates/airplay-client/src/group.rs` is metadata only. It is
not the streaming implementation. The actual group streaming code is in
`client.rs`, `connection.rs`, and `airplay-audio/src/streamer.rs`.

## Target data flow

```text
WASAPI loopback (one thread)
        |
        v
LiveFrameSender (bounded channel)
        |
        v
LiveAudioDecoder -> shared PCM buffer -> optional shared EQ -> one ALAC encoder
        |
        v
one encoded ALAC payload + common RTP timestamp/PTP media time
        |
        +-> RtpSender A: own seq/cipher/history/socket -> Receiver A
        +-> RtpSender B: own seq/cipher/history/socket -> Receiver B
        +-> RtpSender N: own seq/cipher/history/socket -> Receiver N
```

The ALAC payload is shareable. The full wire packet is not: each SETUP phase 2
returns a receiver-specific `shk`, so `RtpSender::prepare_audio()` must create a
separately encrypted wire packet and maintain receiver-specific retransmission
history. All targets use the same encoded samples, RTP media timestamp, PTP
clock identity, and scheduling deadline.

## Timing architecture

For a HomePod group the primary receiver is the effective PTP grandmaster after
BMCA yield. One coordinator listens on UDP 319/320 and publishes `ClockOffset`
updates. Secondary RTSP sessions mirror this watch channel instead of creating
independent PTP clients. `AudioStreamer` applies the offset to the common media
time and sends PT=87 sync packets with the primary clock identity.

This is the authoritative implemented path. `run_ptp_group_master_flow()` also
exists as an experimental sender-grandmaster flow but is not used by
`AirPlayClient::connect_group()` and is not selected for the Windows MVP.

The initial buffer fill acts as the current start barrier: all RTSP sessions are
set up before the shared `AudioStreamer` starts, then it waits for approximately
half of its 2-second buffer. There is not yet an explicit per-receiver READY
acknowledgement or configurable common future epoch exposed at the app layer.

## Integration strategy

1. Preserve the WASAPI capture thread and the Windows portability patches.
2. Add an app-level selected-group model outside tray event handlers.
3. For two or more devices, replace the single `Connection` path with
   `AirPlayClient::connect_group()` and one shared live decoder/capture source.
4. Retain the proven single-device path for one selected receiver.
5. Make tray checkboxes independent and restart the negotiated group when the
   selected set changes. This is an upstream API limitation, not a claim that
   dynamic join is impossible in the AirPlay protocol.
6. Keep per-target transport failures isolated in the existing sender loop;
   add full receiver lifecycle/reconnect coordination in the next milestone.

## Baseline quality finding

`cargo build --workspace` succeeds on Rust 1.98.0/MSVC. The unchanged
`cargo test --workspace --all-targets` does not compile because commit
`b0234c3` made `AacEncoder` feature-gated but left AAC-only tests enabled when
the feature is off. The test modules must receive the same feature gate before
the baseline can be green.

## Known technical risks

- Ports 319/320 may be unavailable or blocked; current PTP code falls back to
  ephemeral ports, which may not interoperate with HomePod.
- `connect_group()` connects receivers serially and aborts on the first setup
  failure. It does not yet return partial success information.
- The group control-listener thread has no explicit stop token and polls until
  sockets close; lifecycle tests are required before claiming leak-free restarts.
- Dynamic join/remove is not implemented by the streaming group API. The
  metadata-only `add_to_group()`/`remove_from_group()` methods are insufficient.
- Windows uses `spin_sleep`; no Windows MMCSS/thread-priority integration exists.
- Discovery is currently one-shot, so rediscovery and interface-change handling
  remain later work.
- Pairing material persistence/Windows Credential Manager support is not in the
  Windows app yet.
- No physical multi-HomePod validation has been performed in this checkout.

