# Architecture

Internal design and protocols of RJ45 Sound Card (RJSC).

## Overview

RJSC is a network audio bridge composed of two roles — **server** and **client** — that communicate over three protocols on three distinct ports.

```
┌───────────────────┐                           ┌───────────────────┐
│     SERVER        │                           │      CLIENT       │
│                   │◄─────────────────────────►│                   │
│  ┌─────────────┐  │   UDP 42001 (audio)       │  ┌─────────────┐  │
│  │ cpal capture │──┼───────────────────────────┼─►│ cpal output  │  │
│  │ cpal output  │◄─┼───────────────────────────┼──│ cpal capture │  │
│  └─────────────┘  │                           │  └─────────────┘  │
│                   │   TCP 42002 (control)      │                   │
│  ┌─────────────┐  │◄─────────────────────────►│  ┌─────────────┐  │
│  │ JSON handler │  │                           │  │ JSON handler │  │
│  └─────────────┘  │                           │  └─────────────┘  │
│                   │   UDP 42000 (discovery)     │                   │
│  ┌─────────────┐  │──► 255.255.255.255 ───────►│  ┌─────────────┐  │
│  │ broadcast   │  │    every 3 seconds          │  │ listener    │  │
│  └─────────────┘  │                           │  └─────────────┘  │
└───────────────────┘                           └───────────────────┘
```

## Thread Model

RJSC uses a hybrid async + thread model:

```
main thread (tokio async)
├── discovery broadcast task (tokio::spawn)
├── audio capture → network task (tokio::spawn)
├── network → audio playback task (tokio::spawn)
├── TCP accept loop (main task)
│   └── per-client handler tasks (tokio::spawn)
│
GUI thread (separate, eframe)
└── backend Tokio runtime (std::thread::spawn)
    ├── server sub-thread (std::thread::spawn → Tokio runtime)
    └── client event loop (same runtime)
```

Audio data flows through `crossbeam` bounded channels (capacity 32) between the audio thread (cpal callbacks) and the network thread (tokio async tasks).

## Audio Pipeline

```
Capture (cpal callback, real-time thread)
    │  f32 samples
    ▼
crossbeam::channel (bounded, 32 slots)
    │  Vec<f32>
    ▼
tokio::spawn task (async)
    │  Apply per-channel volume (RwLock<Vec<f32>>)
    │  Build AudioPacketHeader (40-byte binary)
    │  Serialize as: header_bytes + sample_le_bytes
    ▼
UdpSocket::send_to()  ─────  network  ─────►  UdpSocket::recv_from()
                                                   │
                                                   ▼
                                              Parse header (validate magic 0x524A3435)
                                                   │
                                                   ▼
                                              crossbeam::channel (bounded, 32 slots)
                                                   │  Vec<f32>
                                                   ▼
                                              Playback (cpal callback, real-time thread)
                                                   │  Write to output buffer or fill silence
```

### AudioPacketHeader (40 bytes, little-endian)

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 4 | magic | Constant `0x524A3435` = ASCII "RJ45" |
| 4 | 4 | stream_id | Assigned by server, increments per connection |
| 8 | 8 | sequence | Monotonically increasing per stream |
| 16 | 8 | timestamp_us | Microseconds (reserved, always 0) |
| 24 | 2 | channels | Number of audio channels |
| 26 | 4 | sample_rate | Sample rate in Hz |
| 30 | 1 | sample_format | 0 = f32, 1 = i16, 2 = i24, 3 = i32 |
| 31 | 4 | frame_count | Number of frames in this packet |
| 35 | 1 | _reserved | Padding |
| 36 | 4 | data_len | Bytes of audio data following header |

The audio data follows immediately: `frame_count * channels * 4` bytes of little-endian f32 PCM.

## Control Protocol (TCP 42002)

JSON messages, each prefixed by a 4-byte little-endian length field.

### Message Types

| Message | Direction | Fields |
|---------|-----------|--------|
| `list_devices` | Client → Server | (none) |
| `device_list` | Server → Client | `devices: [DeviceInfo]` |
| `select_device` | Client → Server | `device_id`, `channels`, `sample_rate` |
| `device_selected` | Server → Client | `device_id`, `channels`, `sample_rate`, `stream_id` |
| `start_stream` | Client → Server | `direction: "both"` |
| `stop_stream` | Client → Server | (none) |
| `status` | Server → Client | `running`, `uptime_secs`, `clients`, `stream_id`, `audio_{rx,tx}_kbps` |
| `set_volume` | Client → Server | `channel: u16`, `volume: f32` |
| `error` | Server → Client | `message: string` |
| `ping` | Client → Server | (none) |
| `pong` | Server → Client | (none) |

### DeviceInfo

```json
{
  "index": 0,
  "name": "MOTU 424",
  "input_channels": 24,
  "output_channels": 24,
  "sample_rates": [44100, 48000, 96000, 192000]
}
```

### Typical Session Flow

```
Client                              Server
  │                                    │
  │──── TCP connect ──────────────────►│
  │──── list_devices ─────────────────►│
  │◄─── device_list ──────────────────│
  │──── select_device ────────────────►│
  │◄─── device_selected ──────────────│  (stream_id assigned)
  │──── start_stream {"direction":"both"}►│
  │◄─── status {"running":true} ──────│
  │                                    │
  │◄═══ UDP audio (42001) ════════════│  (continuous)
  │════ UDP audio (42001) ════════════►│  (bidirectional)
  │                                    │
  │──── ping (every 10s) ─────────────►│
  │◄─── pong ─────────────────────────│
  │                                    │
  │──── stop_stream ──────────────────►│
  │──── TCP close ────────────────────►│
```

## Discovery (UDP 42000)

The server broadcasts a JSON payload to `255.255.255.255:42000` every 3 seconds:

```json
{
  "hostname": "studio-pc",
  "device_name": "MOTU 424",
  "device_channels": 24,
  "audio_port": 42001,
  "control_port": 42002,
  "protocol_version": "1.0.0"
}
```

The client listens on `0.0.0.0:42000` for the configured timeout (default 5s) and collects all unique servers (deduplicated by socket address).

## Configuration System

Configuration is loaded from a TOML file (`rjsc.toml` by default). If the file doesn't exist, hardcoded defaults are used. The `Settings` struct is deserialized via `serde` + `toml`.

See [docs/config.md](docs/config.md) for the full reference.

## Key Dependencies

| Crate | Purpose |
|-------|---------|
| `cpal` | Cross-platform audio capture/playback (ALSA/CoreAudio/WASAPI) |
| `tokio` | Async runtime for network I/O |
| `crossbeam` | Fast MPMC channels for audio buffer passing |
| `clap` | CLI argument parsing with derive macros |
| `serde`/`serde_json` | JSON serialization for control protocol |
| `toml` | Configuration file parsing |
| `eframe`/`egui` | Optional GUI control panel |
| `anyhow` | Error handling |
| `log`/`env_logger` | Structured logging |

## Build and Packaging

`scripts/build-release.sh` orchestrates the full release pipeline:

1. Builds Linux binary (`cargo build --release --features gui`)
2. Cross-compiles Windows binary (`--target x86_64-pc-windows-gnu --no-default-features`)
3. Generates role-specific wrappers (`rjsc-server`, `rjsc-client`, `.bat` files)
4. Generates role-specific configs (`rjsc-server.toml`, `rjsc-client.toml`)
5. Generates the man page (`rjsc.1`)
6. Packages into DEB, RPM, Arch Linux, Windows zip, and macOS zip formats

## Thread Safety

- `crossbeam::channel` for audio data (lock-free MPMC, suitable for real-time callbacks)
- `std::sync::atomic::{AtomicU32, AtomicU64}` for shared counters (stream ID, sequence, client count)
- `tokio::sync::Mutex` for `current_client_addr` (async-compatible lock)
- `tokio::sync::RwLock` for per-channel volumes (many readers, few writers)
- `Arc` for shared ownership of sockets and state across tasks

## Limitations

- Single active audio stream at a time (one client's UDP packets are processed)
- No encryption or authentication
- No jitter buffer or packet loss recovery (UDP is fire-and-forget)
- Virtual device name must be configured manually per platform
- Server binds to a single network interface (`bind_address`)
- Audio format is always 32-bit float (no integer PCM support for streaming)
