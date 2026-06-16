use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AudioFormat {
    pub channels: u16,
    pub sample_rate: u32,
    pub buffer_frames: usize,
}

impl AudioFormat {
    pub fn new(channels: u16, sample_rate: u32, buffer_frames: usize) -> Self {
        Self { channels, sample_rate, buffer_frames }
    }

    pub fn bytes_per_sample(&self) -> usize {
        4
    }

    pub fn bytes_per_frame(&self) -> usize {
        self.channels as usize * self.bytes_per_sample()
    }

    pub fn buffer_bytes(&self) -> usize {
        self.buffer_frames * self.bytes_per_frame()
    }

    pub fn bitrate_mbps(&self) -> f64 {
        self.channels as f64 * self.sample_rate as f64 * self.bytes_per_sample() as f64 * 8.0 / 1_000_000.0
    }
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self {
            channels: 2,
            sample_rate: 48000,
            buffer_frames: 256,
        }
    }
}
