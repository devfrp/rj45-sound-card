# Changelog

All notable changes to RJ45 Sound Card are documented here.

## [1.0.0] — 2026-06-16

### Added
- Initial release
- Server mode: share local audio devices over Ethernet/RJ45
- Client mode: connect to remote server and receive audio as a local virtual device
- Bidirectional audio streaming (server → client and client → server)
- UDP audio transport with custom 40-byte binary header (port 42001)
- TCP JSON control protocol (port 42002)
- UDP broadcast auto-discovery (port 42000, every 3 seconds)
- TOML configuration file (`rjsc.toml`) with `rjsc init`
- Graphical control panel (egui/eframe, feature-gated)
- Per-channel volume control (0.0–1.0 slider)
- Multi-channel support (1–64+ channels)
- Configurable sample rates: 44.1, 48, 96, 192 kHz
- Configurable buffer sizes: 64, 128, 256, 512, 1024 frames
- Auto-reconnect with configurable delay (client mode)
- Max clients limit (server mode)
- Keep-alive ping/pong over TCP
- Virtual audio device setup scripts:
  - Linux: ALSA snd-aloop
  - macOS: BlackHole
  - Windows: VB-Cable
- Multi-distro packaging: DEB, RPM, Arch Linux, Windows zip, macOS zip
- Man page (`rjsc.1`)
- Cross-compilation for Windows (headless)
- ARM64 support (Apple Silicon native)

[1.0.0]: https://github.com/devfrp/rj45-sound-card/releases/tag/v1.0.0
