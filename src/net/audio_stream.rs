use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;

pub const DEFAULT_AUDIO_PORT: u16 = 42001;
pub const MAX_PACKET_SIZE: usize = 65507; // max UDP payload

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioPacketHeader {
    pub magic: u32,         // 0x524A3435 = "RJ45"
    pub stream_id: u32,
    pub sequence: u64,
    pub timestamp_us: u64,
    pub channels: u16,
    pub sample_rate: u32,
    pub sample_format: u8,  // 0=f32, 1=i16, 2=i24, 3=i32
    pub _reserved: u8,
    pub frame_count: u32,
    pub data_len: u32,      // bytes of audio data following header
}

impl AudioPacketHeader {
    pub const MAGIC: u32 = 0x524A3435;
    pub const SIZE: usize = 40;

    pub fn new(stream_id: u32, sequence: u64, channels: u16, sample_rate: u32, frame_count: u32) -> Self {
        Self {
            magic: Self::MAGIC,
            stream_id,
            sequence,
            timestamp_us: 0,
            channels,
            sample_rate,
            sample_format: 0, // f32
            _reserved: 0,
            frame_count,
            data_len: frame_count * channels as u32 * 4,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::SIZE);
        buf.extend_from_slice(&self.magic.to_le_bytes());
        buf.extend_from_slice(&self.stream_id.to_le_bytes());
        buf.extend_from_slice(&self.sequence.to_le_bytes());
        buf.extend_from_slice(&self.timestamp_us.to_le_bytes());
        buf.extend_from_slice(&self.channels.to_le_bytes());
        buf.extend_from_slice(&self.sample_rate.to_le_bytes());
        buf.extend_from_slice(&self.sample_format.to_le_bytes());
        buf.push(self._reserved);
        buf.extend_from_slice(&self.frame_count.to_le_bytes());
        buf.extend_from_slice(&self.data_len.to_le_bytes());
        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let magic = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
        if magic != Self::MAGIC {
            return None;
        }
        Some(Self {
            magic,
            stream_id: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
            sequence: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
            timestamp_us: u64::from_le_bytes(bytes[16..24].try_into().ok()?),
            channels: u16::from_le_bytes(bytes[24..26].try_into().ok()?),
            sample_rate: u32::from_le_bytes(bytes[26..30].try_into().ok()?),
            sample_format: bytes[30],
            _reserved: bytes[31],
            frame_count: u32::from_le_bytes(bytes[32..36].try_into().ok()?),
            data_len: u32::from_le_bytes(bytes[36..40].try_into().ok()?),
        })
    }
}

pub async fn send_audio_packet(
    socket: &UdpSocket,
    addr: &std::net::SocketAddr,
    header: &AudioPacketHeader,
    audio_data: &[f32],
) -> Result<()> {
    let hdr_bytes = header.to_bytes();
    let sample_bytes: Vec<u8> = audio_data
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();

    let mut packet = Vec::with_capacity(hdr_bytes.len() + sample_bytes.len());
    packet.extend_from_slice(&hdr_bytes);
    packet.extend_from_slice(&sample_bytes);

    socket.send_to(&packet, addr).await?;
    Ok(())
}

pub fn parse_audio_packet(data: &[u8]) -> Option<(AudioPacketHeader, Vec<f32>)> {
    let header = AudioPacketHeader::from_bytes(data)?;
    let audio_start = AudioPacketHeader::SIZE;
    if data.len() < audio_start + header.data_len as usize {
        return None;
    }
    let raw = &data[audio_start..audio_start + header.data_len as usize];
    let samples: Vec<f32> = raw
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Some((header, samples))
}
