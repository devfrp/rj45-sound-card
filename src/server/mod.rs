use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use crossbeam::channel;
use cpal::traits::DeviceTrait;
use tokio::io::BufReader;

use crate::audio::capture::AudioCapture;
use crate::audio::playback::AudioPlayback;
use crate::config::Settings;
use crate::net::audio_stream::{self, AudioPacketHeader};
use crate::net::control::{self, ControlMessage, DeviceInfo};
use crate::net::discovery;

pub async fn run(settings: Settings, stop_flag: Arc<AtomicBool>) -> Result<()> {
    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown".to_string());

    log::info!("RJ45 Sound Card - Server Mode");
    log::info!("Hostname: {}", hostname);

    let input_device = if settings.audio.input_device == "@default" {
        crate::audio::default_input_device()?
    } else {
        crate::audio::find_device(&settings.audio.input_device)?
    };
    let output_device = if settings.audio.output_device == "@default" {
        crate::audio::default_output_device()?
    } else {
        crate::audio::find_device(&settings.audio.output_device)?
    };

    log::info!("Input device: {}", input_device.name()?);
    log::info!("Output device: {}", output_device.name()?);

    let audio_format = settings.audio_format();
    log::info!(
        "Audio format: {} channels @ {} Hz, buffer: {} frames",
        audio_format.channels,
        audio_format.sample_rate,
        audio_format.buffer_frames
    );
    log::info!(
        "Estimated bandwidth: {:.1} Mbps",
        audio_format.bitrate_mbps()
    );

    // Audio capture
    let capture = AudioCapture::new(&input_device, &audio_format)?;
    capture.start()?;
    let capture_rx = capture.receiver();
    let actual_format = *capture.format();

    // Audio playback
    let playback = AudioPlayback::new(&output_device, &actual_format)?;
    playback.start()?;
    let playback_tx = playback.sender();

    // Bind UDP audio socket
    let audio_socket = Arc::new(
        tokio::net::UdpSocket::bind(format!(
            "{}:{}",
            settings.network.bind_address,
            settings.network.audio_port
        ))
        .await?,
    );
    let audio_port = audio_socket.local_addr()?.port();

    // Bind TCP control listener
    let audio_port_for_clients = audio_port;
    let (control_listener, control_port) =
        control::run_control_server(&format!(
            "{}:{}",
            settings.network.bind_address,
            settings.network.control_port
        ))
        .await?;

    // Start discovery broadcast
    let device_name = input_device.name().unwrap_or_else(|_| "Unknown".to_string());
    let dn_for_discovery = device_name.clone();
    let hn = hostname.clone();
    let ch = audio_format.channels;
    tokio::spawn(async move {
        if let Err(e) = discovery::announce_server(
            &hn, &dn_for_discovery, ch, audio_port, control_port,
        ).await {
            log::error!("Discovery broadcast failed: {}", e);
        }
    });

    // Shared state
    let connected_clients = Arc::new(AtomicU32::new(0));
    let next_stream_id = Arc::new(AtomicU32::new(1));
    let current_stream_id = Arc::new(AtomicU32::new(0));
    let audio_sequence = Arc::new(AtomicU64::new(0));
    let current_client_addrs: Arc<tokio::sync::RwLock<Vec<std::net::SocketAddr>>> =
        Arc::new(tokio::sync::RwLock::new(Vec::new()));
    let client_audio_port = audio_port_for_clients;

    // Build audio destination from client IP + audio port
    fn audio_addr_for(client: std::net::SocketAddr, port: u16) -> std::net::SocketAddr {
        std::net::SocketAddr::new(client.ip(), port)
    }

    // Per-channel volume multipliers (shared between control handler and audio sender)
    let volumes: Arc<tokio::sync::RwLock<Vec<f32>>> =
        Arc::new(tokio::sync::RwLock::new(vec![1.0; audio_format.channels as usize]));

    // Audio capture -> network task
    let audio_socket_clone = audio_socket.clone();
    let client_addrs = current_client_addrs.clone();
    let sid = current_stream_id.clone();
    let seq = audio_sequence.clone();
    let fmt_ch = audio_format.channels;
    let fmt_sr = audio_format.sample_rate;
    let fmt_bf = audio_format.buffer_frames as u32;
    let vols = volumes.clone();
    tokio::spawn(async move {
        loop {
            match capture_rx.recv() {
                Ok(mut samples) => {
                    let addr_guard = client_addrs.read().await;
                    if !addr_guard.is_empty() {
                        // Apply per-channel volume
                        let vol_guard = vols.read().await;
                        for (i, sample) in samples.iter_mut().enumerate() {
                            let ch_idx = i % fmt_ch as usize;
                            *sample *= vol_guard[ch_idx];
                        }
                        drop(vol_guard);

                        let s = seq.fetch_add(1, Ordering::SeqCst);
                        let header = AudioPacketHeader::new(
                            sid.load(Ordering::SeqCst),
                            s,
                            fmt_ch,
                            fmt_sr,
                            fmt_bf,
                        );
                        for addr in addr_guard.iter() {
                            if let Err(e) = audio_stream::send_audio_packet(
                                &audio_socket_clone,
                                addr,
                                &header,
                                &samples,
                            ).await {
                                log::warn!("Failed to send audio to {}: {}", addr, e);
                            }
                        }
                    }
                }
                Err(channel::RecvError) => {
                    log::error!("Capture channel closed");
                    break;
                }
            }
        }
    });

    // Network -> audio playback task
    let audio_socket_clone2 = audio_socket.clone();
    let client_addr_filter = current_client_addrs.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; audio_stream::MAX_PACKET_SIZE];
        loop {
            match audio_socket_clone2.recv_from(&mut buf).await {
                Ok((len, src_addr)) => {
                    let addr_guard = client_addr_filter.read().await;
                    let allowed = addr_guard.iter().any(|a| a.ip() == src_addr.ip());
                    drop(addr_guard);
                    if !allowed {
                        continue;
                    }
                    if let Some((_header, samples)) = audio_stream::parse_audio_packet(&buf[..len]) {
                        if let Err(e) = playback_tx.try_send(samples) {
                            if !e.is_disconnected() {
                                log::warn!("Playback buffer full, dropping frame");
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

    // Accept client connections
    log::info!("Waiting for client connections on port {}...", control_port);

    loop {
        if stop_flag.load(Ordering::SeqCst) {
            log::info!("Server shutting down...");
            break Ok(());
        }
        let (tcp_stream, client_addr) = control_listener.accept().await?;
        log::info!("Client connected from {}", client_addr);

        let connected = connected_clients.clone();
        let max_clients = settings.server.max_clients;
        let current_addr = current_client_addrs.clone();
        let audio_port = client_audio_port;
        let audio_fmt = actual_format;
        let device_name_str = device_name.clone();
        let sid_counter = next_stream_id.clone();
        let cur_sid = current_stream_id.clone();
        let seq_ctr = audio_sequence.clone();
        let vols = volumes.clone();

        tokio::spawn(async move {
            if connected.load(Ordering::SeqCst) >= max_clients {
                log::warn!("Rejected client {} (max clients reached)", client_addr);
                return;
            }
            connected.fetch_add(1, Ordering::SeqCst);

            let (mut read_half, mut write_half) = tokio::io::split(tcp_stream);
            let mut reader = BufReader::new(&mut read_half);

            loop {
                match control::recv_control(&mut reader).await {
                    Ok(msg) => {
                        match msg {
                            ControlMessage::ListDevices => {
                                let devices = vec![DeviceInfo {
                                    index: 0,
                                    name: device_name_str.clone(),
                                    input_channels: audio_fmt.channels,
                                    output_channels: audio_fmt.channels,
                                    sample_rates: vec![44100, 48000, 96000, 192000],
                                }];
                                if let Err(e) = control::send_control(
                                    &mut write_half,
                                    &ControlMessage::DeviceList { devices },
                                ).await {
                                    log::error!("Failed to send device list: {}", e);
                                    break;
                                }
                            }
                            ControlMessage::SelectDevice { channels, sample_rate, .. } => {
                                let sid = sid_counter.fetch_add(1, Ordering::SeqCst);
                                cur_sid.store(sid, Ordering::SeqCst);
                                seq_ctr.store(0, Ordering::SeqCst);
                                let new_addr = audio_addr_for(client_addr, audio_port);
                                {
                                    let mut addrs = current_addr.write().await;
                                    if !addrs.iter().any(|a| a == &new_addr) {
                                        addrs.push(new_addr);
                                    }
                                }
                                if let Err(e) = control::send_control(
                                    &mut write_half,
                                    &ControlMessage::DeviceSelected {
                                        device_id: 0,
                                        channels,
                                        sample_rate,
                                        stream_id: sid,
                                    },
                                ).await {
                                    log::error!("Failed to confirm device: {}", e);
                                    break;
                                }
                                log::info!(
                                    "Client {} selected device: {} ch @ {} Hz (stream #{})",
                                    client_addr, channels, sample_rate, sid
                                );
                            }
                            ControlMessage::StartStream { direction } => {
                                log::info!("Client {} started {} stream", client_addr, direction);
                                if let Err(e) = control::send_control(
                                    &mut write_half,
                                    &ControlMessage::Status {
                                        running: true,
                                        uptime_secs: 0,
                                        clients: connected.load(Ordering::SeqCst),
                                        stream_id: None,
                                        audio_rx_kbps: 0.0,
                                        audio_tx_kbps: 0.0,
                                    },
                                ).await {
                                    log::error!("Failed to send status: {}", e);
                                    break;
                                }
                            }
                            ControlMessage::StopStream => {
                                log::info!("Client {} stopped stream", client_addr);
                                current_addr.write().await.retain(|a| a.ip() != client_addr.ip());
                            }
                            ControlMessage::SetVolume { channel, volume } => {
                                let mut vol_guard = vols.write().await;
                                let ch_idx = channel as usize;
                                if ch_idx < vol_guard.len() {
                                    vol_guard[ch_idx] = volume.clamp(0.0, 1.0);
                                    log::info!(
                                        "Client {} set channel {} volume to {:.2}",
                                        client_addr, channel, volume
                                    );
                                }
                            }
                            ControlMessage::Ping => {
                                if let Err(e) = control::send_control(
                                    &mut write_half, &ControlMessage::Pong,
                                ).await {
                                    log::error!("Failed to send pong: {}", e);
                                    break;
                                }
                            }
                            _ => {
                                log::warn!("Unexpected message from {}: {:?}", client_addr, msg);
                            }
                        }
                    }
                    Err(e) => {
                        log::info!("Client {} disconnected: {}", client_addr, e);
                        break;
                    }
                }
            }

            connected.fetch_sub(1, Ordering::SeqCst);
            current_addr.write().await.retain(|a| a.ip() != client_addr.ip());
            log::info!("Client {} removed", client_addr);
        });
    }
}
