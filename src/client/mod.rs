use anyhow::Result;
use crossbeam::channel;
use cpal::traits::DeviceTrait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::BufReader;
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::{sleep, Duration};

use crate::audio::capture::AudioCapture;
use crate::audio::format::AudioFormat;
use crate::audio::playback::AudioPlayback;
use crate::config::Settings;
use crate::net::audio_stream::{self, AudioPacketHeader};
use crate::net::control::{self, ControlMessage};
use crate::net::discovery;

pub async fn run(settings: Settings, server_addr: Option<String>, stop_flag: Arc<AtomicBool>) -> Result<()> {
    log::info!("RJ45 Sound Card - Client Mode");

    // Resolve server address once (outside reconnect loop)
    let (server_addr, discovered_audio_port) = if let Some(addr) = server_addr {
        (addr, None)
    } else if !settings.client.server_address.is_empty() {
        (settings.client.server_address.clone(), None)
    } else {
        log::info!("Discovering servers on the network...");
        let servers = discovery::discover_servers(5).await?;
        if servers.is_empty() {
            anyhow::bail!("No servers found. Specify --server or configure server_address.");
        }
        let (addr, msg) = &servers[0];
        log::info!(
            "Connecting to {} at {} (device: {}, audio port {})",
            msg.hostname, addr, msg.device_name, msg.audio_port
        );
        (format!("{}:{}", addr.ip(), msg.control_port), Some(msg.audio_port))
    };

    let server_ip = server_addr
        .rsplitn(2, ':')
        .last()
        .unwrap_or("127.0.0.1")
        .trim_matches(|c| c == '[' || c == ']')
        .to_string();

    loop {
        match run_session(&settings, &server_addr, &server_ip, discovered_audio_port, &stop_flag).await {
            Ok(()) => {
                if settings.client.auto_reconnect {
                    log::info!("Session ended, reconnecting in 5 seconds...");
                    sleep(Duration::from_secs(5)).await;
                    continue;
                }
                return Ok(());
            }
            Err(e) => {
                if settings.client.auto_reconnect {
                    log::error!("Session error: {}. Reconnecting in 5 seconds...", e);
                    sleep(Duration::from_secs(5)).await;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

async fn run_session(settings: &Settings, server_addr: &str, server_ip: &str, discovered_audio_port: Option<u16>, stop_flag: &AtomicBool) -> Result<()> {
    log::info!("Connecting to server at {}...", server_addr);
    let tcp_stream = TcpStream::connect(server_addr).await?;
    let (mut read_half, mut write_half) = tokio::io::split(tcp_stream);
    let mut reader = BufReader::new(&mut read_half);
    log::info!("Connected to server");

    control::send_control(&mut write_half, &ControlMessage::ListDevices).await?;
    log::info!("Requested device list...");

    let device_list = loop {
        match control::recv_control(&mut reader).await? {
            ControlMessage::DeviceList { devices } => {
                log::info!("Available devices on server:");
                for (i, d) in devices.iter().enumerate() {
                    log::info!(
                        "  {}. {} ({} in / {} out, rates: {:?})",
                        i, d.name, d.input_channels, d.output_channels, d.sample_rates
                    );
                }
                break devices;
            }
            other => {
                log::warn!("Unexpected message: {:?}", other);
            }
        }
    };

    if device_list.is_empty() {
        anyhow::bail!("No audio devices available on server");
    }

    let device = &device_list[0];
    let channels = settings.audio.channels.min(device.input_channels);
    let sample_rate = settings.audio.sample_rate;

    control::send_control(
        &mut write_half,
        &ControlMessage::SelectDevice {
            device_id: device.index,
            channels,
            sample_rate,
        },
    )
    .await?;

    loop {
        match control::recv_control(&mut reader).await? {
            ControlMessage::DeviceSelected { stream_id, .. } => {
                log::info!("Device confirmed (stream #{})", stream_id);
                break;
            }
            ControlMessage::Error { message } => {
                anyhow::bail!("Server error: {}", message);
            }
            _ => {}
        }
    }

    let audio_format = AudioFormat::new(channels, sample_rate, settings.audio.buffer_frames);

    // Set up playback device
    let output_device = if settings.client.use_virtual_device
        && !settings.client.virtual_device_name.is_empty()
    {
        match crate::audio::find_device(&settings.client.virtual_device_name) {
            Ok(d) => {
                log::info!("Using virtual audio device: {}", d.name()?);
                d
            }
            Err(_) => {
                log::warn!(
                    "Virtual device '{}' not found, using default output",
                    settings.client.virtual_device_name
                );
                crate::audio::default_output_device()?
            }
        }
    } else {
        crate::audio::default_output_device()?
    };

    let playback = AudioPlayback::new(&output_device, &audio_format)?;
    playback.start()?;
    let playback_tx = playback.sender();

    // Set up local capture
    let input_device = crate::audio::default_input_device()?;
    let capture = AudioCapture::new(&input_device, &audio_format)?;
    capture.start()?;
    let capture_rx = capture.receiver();

    // Start stream
    control::send_control(
        &mut write_half,
        &ControlMessage::StartStream {
            direction: "both".to_string(),
        },
    )
    .await?;

    // Server audio address
    let audio_port = discovered_audio_port.unwrap_or(settings.network.audio_port);
    let server_audio_addr: std::net::SocketAddr =
        format!("{}:{}", server_ip, audio_port).parse()?;

    // Receive audio from server -> local playback
    let rx_sock = UdpSocket::bind(format!("0.0.0.0:{}", settings.network.audio_port)).await?;
    let rx_tx = playback_tx.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; audio_stream::MAX_PACKET_SIZE];
        loop {
            match rx_sock.recv_from(&mut buf).await {
                Ok((len, _addr)) => {
                    if let Some((_header, samples)) = audio_stream::parse_audio_packet(&buf[..len]) {
                        if let Err(e) = rx_tx.try_send(samples) {
                            if e.is_full() {
                                log::warn!("Playback buffer full, dropping audio");
                            } else {
                                log::error!("Playback channel disconnected");
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    log::error!("Audio receive error: {}", e);
                    break;
                }
            }
        }
    });

    // Local capture -> send to server
    let tx_sock = UdpSocket::bind("0.0.0.0:0").await?;
    let ch = audio_format.channels;
    let sr = audio_format.sample_rate;
    let bf = audio_format.buffer_frames as u32;
    tokio::spawn(async move {
        let mut sequence: u64 = 0;
        loop {
            match capture_rx.recv() {
                Ok(samples) => {
                    let header = AudioPacketHeader::new(0, sequence, ch, sr, bf);
                    sequence = sequence.wrapping_add(1);
                    if let Err(e) = audio_stream::send_audio_packet(
                        &tx_sock,
                        &server_audio_addr,
                        &header,
                        &samples,
                    ).await {
                        log::error!("Failed to send capture: {}", e);
                        break;
                    }
                }
                Err(channel::RecvError) => {
                    log::error!("Capture channel closed");
                    break;
                }
            }
        }
    });

    // Keep-alive ping loop
    loop {
        if stop_flag.load(Ordering::SeqCst) {
            log::info!("Session stopped cleanly");
            return Ok(());
        }
        sleep(Duration::from_secs(10)).await;
        if let Err(e) = control::send_control(&mut write_half, &ControlMessage::Ping).await {
            log::info!("Keep-alive lost: {}", e);
            break;
        }
    }

    Ok(())
}
