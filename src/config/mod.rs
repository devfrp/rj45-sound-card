use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::audio::format::AudioFormat;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Audio format configuration
    pub audio: AudioSettings,

    /// Network configuration
    pub network: NetworkSettings,

    /// Server-specific settings
    pub server: ServerSettings,

    /// Client-specific settings
    pub client: ClientSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSettings {
    /// Input device name (or index) for server mode
    pub input_device: String,

    /// Output device name (or index) for playback
    pub output_device: String,

    /// Number of audio channels
    pub channels: u16,

    /// Sample rate in Hz (44100, 48000, 96000, 192000)
    pub sample_rate: u32,

    /// Buffer size in frames (power of 2: 64, 128, 256, 512, 1024)
    pub buffer_frames: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkSettings {
    /// Audio streaming UDP port
    pub audio_port: u16,

    /// Control TCP port
    pub control_port: u16,

    /// Discovery UDP port
    pub discovery_port: u16,

    /// Bind address
    pub bind_address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerSettings {
    /// Whether to auto-accept client connections
    pub auto_accept: bool,

    /// Maximum number of connected clients
    pub max_clients: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientSettings {
    /// Server address (auto-discover if empty)
    pub server_address: String,

    /// Whether to use the virtual audio device
    pub use_virtual_device: bool,

    /// Virtual device name to use
    pub virtual_device_name: String,

    /// Auto-reconnect on disconnect
    pub auto_reconnect: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            audio: AudioSettings {
                input_device: "@default".to_string(),
                output_device: "@default".to_string(),
                channels: 2,
                sample_rate: 48000,
                buffer_frames: 256,
            },
            network: NetworkSettings {
                audio_port: crate::net::audio_stream::DEFAULT_AUDIO_PORT,
                control_port: crate::net::control::DEFAULT_CONTROL_PORT,
                discovery_port: crate::net::discovery::DISCOVERY_PORT,
                bind_address: "0.0.0.0".to_string(),
            },
            server: ServerSettings {
                auto_accept: true,
                max_clients: 4,
            },
            client: ClientSettings {
                server_address: String::new(),
                use_virtual_device: true,
                virtual_device_name: "RJ45 Virtual Audio".to_string(),
                auto_reconnect: true,
            },
        }
    }
}

impl Settings {
    pub fn audio_format(&self) -> AudioFormat {
        AudioFormat::new(self.audio.channels, self.audio.sample_rate, self.audio.buffer_frames)
    }
}

pub fn load(path: &str) -> Result<Settings> {
    if !Path::new(path).exists() {
        log::info!("Config file '{}' not found, using defaults", path);
        return Ok(Settings::default());
    }
    let content = std::fs::read_to_string(path)?;
    let settings: Settings = toml::from_str(&content)?;
    log::info!("Loaded configuration from '{}'", path);
    Ok(settings)
}

pub fn save_default(path: &str) -> Result<()> {
    let settings = Settings::default();
    let content = toml::to_string_pretty(&settings)?;
    std::fs::write(path, content)?;
    log::info!("Saved default configuration to '{}'", path);
    Ok(())
}
