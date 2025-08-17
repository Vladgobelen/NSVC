use std::ffi::CStr;
use std::os::raw::{c_char, c_void};
use std::sync::{Arc, Mutex};
use std::net::{UdpSocket, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering, AtomicU32};
use std::thread;
use std::time::{Duration, Instant};
use std::io::Write;
use std::collections::VecDeque;
use chrono::Utc;
use cpal::{
    traits::{HostTrait, DeviceTrait, StreamTrait},
    StreamConfig, SampleRate, SampleFormat, SupportedStreamConfig
};
use opus::{Encoder, Decoder, Channels, Application, Bitrate};

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: Channels = Channels::Mono;
const FRAME_SIZE: usize = 480;
const BUFFER_DURATION_MS: u32 = 200;
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(1);
const MAX_PACKET_SIZE: usize = 4000;

// Вычисляем размер буфера во время компиляции
const BUFFER_SAMPLES: usize = (SAMPLE_RATE as usize * BUFFER_DURATION_MS as usize) / 1000;

#[repr(C)]
pub struct VoiceClient {
    is_transmitting: Arc<AtomicBool>,
    socket: Arc<UdpSocket>,
    server_addr: SocketAddr,
    running: Arc<AtomicBool>,
    input_stream: Mutex<Option<cpal::Stream>>,
    output_stream: Mutex<Option<cpal::Stream>>,
    pcm_accumulator: Arc<Mutex<Vec<f32>>>,
    encoder: Arc<Mutex<Encoder>>,
    playback_buffer: Arc<Mutex<VecDeque<f32>>>,
    bitrate: Arc<AtomicU32>,
}

// Коды ошибок
pub mod error_codes {
    pub const SUCCESS: i32 = 0;
    pub const NULL_POINTER: i32 = -1;
    pub const INVALID_IP: i32 = -2;
    pub const SOCKET_BIND_FAILED: i32 = -3;
    pub const INVALID_SERVER_ADDR: i32 = -4;
    pub const SOCKET_CONNECT_FAILED: i32 = -5;
    pub const NO_INPUT_DEVICE: i32 = -6;
    pub const NO_OUTPUT_DEVICE: i32 = -7;
    pub const ENCODER_INIT_FAILED: i32 = -8;
    pub const INPUT_STREAM_FAILED: i32 = -9;
    pub const OUTPUT_STREAM_FAILED: i32 = -10;
    pub const INVALID_AUDIO_PARAM: i32 = -11;
    pub const NOT_RUNNING: i32 = -12;
    pub const UNSUPPORTED_SAMPLE_FORMAT: i32 = -13;
}

fn log_message(message: &str) {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S");
    let log_entry = format!("[{}] {}", now, message);
    println!("{}", log_entry);
    
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open("voice_client.log") 
    {
        let _ = writeln!(file, "{}", log_entry);
    }
}

#[no_mangle]
pub extern "C" fn voice_client_new(server_ip: *const c_char, server_port: u16) -> *mut c_void {
    let ip_str = unsafe { CStr::from_ptr(server_ip).to_str().unwrap_or_default() };
    if ip_str.is_empty() {
        log_message("Invalid server IP address");
        return std::ptr::null_mut();
    }
    
    let server_addr_str = format!("{}:{}", ip_str, server_port);
    
    log_message(&format!("Creating client for server: {}", server_addr_str));
    
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => {
            log_message(&format!("Socket bind error: {}", e));
            return std::ptr::null_mut();
        },
    };
    
    if let Err(e) = socket.connect(&server_addr_str) {
        log_message(&format!("Socket connect error: {}", e));
        return std::ptr::null_mut();
    }
    
    if let Err(e) = socket.set_nonblocking(true) {
        log_message(&format!("Set nonblocking error: {}", e));
        return std::ptr::null_mut();
    }
    
    match socket.local_addr() {
        Ok(addr) => log_message(&format!("Socket local address: {}", addr)),
        Err(e) => log_message(&format!("Failed to get local address: {}", e)),
    }
    
    let server_addr = match socket.peer_addr() {
        Ok(addr) => {
            log_message(&format!("Socket connected to: {}", addr));
            addr
        },
        Err(e) => {
            log_message(&format!("Failed to get peer address: {}", e));
            return std::ptr::null_mut();
        }
    };
    
    let mut encoder = match Encoder::new(SAMPLE_RATE, CHANNELS, Application::Voip) {
        Ok(enc) => enc,
        Err(e) => {
            log_message(&format!("Encoder creation error: {:?}", e));
            return std::ptr::null_mut();
        }
    };
    
    // Установка VBR для качественной передачи голоса
    if let Err(e) = encoder.set_bitrate(Bitrate::Bits(64000)) {
        log_message(&format!("Failed to set bitrate: {:?}", e));
    }
    if let Err(e) = encoder.set_vbr(true) {
        log_message(&format!("Failed to set VBR: {:?}", e));
    }
    
    // Инициализация буфера воспроизведения как VecDeque
    let buffer_capacity = (SAMPLE_RATE * BUFFER_DURATION_MS / 1000) as usize;
    let playback_buffer = VecDeque::with_capacity(buffer_capacity);
    
    let client = Box::new(VoiceClient {
        is_transmitting: Arc::new(AtomicBool::new(false)),
        socket: Arc::new(socket),
        server_addr,
        running: Arc::new(AtomicBool::new(false)),
        input_stream: Mutex::new(None),
        output_stream: Mutex::new(None),
        pcm_accumulator: Arc::new(Mutex::new(Vec::new())),
        encoder: Arc::new(Mutex::new(encoder)),
        playback_buffer: Arc::new(Mutex::new(playback_buffer)),
        bitrate: Arc::new(AtomicU32::new(64000)),
    });
    
    Box::into_raw(client) as *mut c_void
}

#[no_mangle]
pub extern "C" fn voice_client_start(client: *mut c_void) -> i32 {
    if client.is_null() {
        log_message("voice_client_start: client is null!");
        return error_codes::NULL_POINTER;
    }
    
    let client = unsafe { &mut *(client as *mut VoiceClient) };
    
    client.running.store(true, Ordering::SeqCst);
    log_message("Starting voice client");
    
    let host = cpal::default_host();
    
    let input_device = match host.default_input_device() {
        Some(dev) => {
            log_message(&format!("Using input device: {:?}", dev.name().unwrap_or_default()));
            dev
        },
        None => {
            log_message("No input device available");
            return error_codes::NO_INPUT_DEVICE;
        }
    };
    
    let output_device = match host.default_output_device() {
        Some(dev) => {
            log_message(&format!("Using output device: {:?}", dev.name().unwrap_or_default()));
            dev
        },
        None => {
            log_message("No output device available");
            return error_codes::NO_OUTPUT_DEVICE;
        }
    };
    
    // Явная конфигурация аудиопотоков
    let stream_config = StreamConfig {
        channels: 1,
        sample_rate: SampleRate(SAMPLE_RATE),
        buffer_size: cpal::BufferSize::Default,
    };
    
    // Проверка поддержки формата f32
    let input_supported = match input_device.supported_input_configs() {
        Ok(mut configs) => configs.any(|c| 
            c.channels() == 1 && 
            c.min_sample_rate() <= SampleRate(SAMPLE_RATE) && 
            c.max_sample_rate() >= SampleRate(SAMPLE_RATE) &&
            c.sample_format() == SampleFormat::F32
        ),
        Err(e) => {
            log_message(&format!("Failed to get input configs: {:?}", e));
            return error_codes::INPUT_STREAM_FAILED;
        }
    };
    
    if !input_supported {
        log_message("Input device does not support required configuration");
        return error_codes::UNSUPPORTED_SAMPLE_FORMAT;
    }
    
    let output_supported = match output_device.supported_output_configs() {
        Ok(mut configs) => configs.any(|c| 
            c.channels() == 1 && 
            c.min_sample_rate() <= SampleRate(SAMPLE_RATE) && 
            c.max_sample_rate() >= SampleRate(SAMPLE_RATE) &&
            c.sample_format() == SampleFormat::F32
        ),
        Err(e) => {
            log_message(&format!("Failed to get output configs: {:?}", e));
            return error_codes::OUTPUT_STREAM_FAILED;
        }
    };
    
    if !output_supported {
        log_message("Output device does not support required configuration");
        return error_codes::UNSUPPORTED_SAMPLE_FORMAT;
    }
    
    let socket_tx = client.socket.clone();
    let socket_rx = client.socket.clone();
    let server_addr = client.server_addr;
    
    let is_transmitting = client.is_transmitting.clone();
    let running = client.running.clone();
    let pcm_accumulator = client.pcm_accumulator.clone();
    let encoder = client.encoder.clone();
    let playback_buffer = client.playback_buffer.clone();
    let bitrate = client.bitrate.clone();

    // Audio input thread
    let running1 = running.clone();
    let input_stream = match input_device.build_input_stream(
        &stream_config,
        move |data: &[f32], _: &_| {
            if !running1.load(Ordering::SeqCst) {
                return;
            }
            
            let transmitting = is_transmitting.load(Ordering::SeqCst);
            if !transmitting {
                return;
            }
            
            let mut acc = match pcm_accumulator.lock() {
                Ok(acc) => acc,
                Err(_) => return,
            };
            
            acc.extend_from_slice(data);
            
            // Process full frames
            while acc.len() >= FRAME_SIZE {
                let frame: Vec<f32> = acc.drain(0..FRAME_SIZE).collect();
                
                // Convert to PCM
                let pcm: Vec<i16> = frame.iter()
                    .map(|&s| {
                        let scaled = s * 32767.0;
                        if scaled > 32767.0 {
                            32767
                        } else if scaled < -32768.0 {
                            -32768
                        } else {
                            scaled as i16
                        }
                    })
                    .collect();
                
                let mut encoder_guard = match encoder.lock() {
                    Ok(enc) => enc,
                    Err(_) => return,
                };
                
                // Применяем текущий битрейт
                let current_bitrate = bitrate.load(Ordering::Relaxed) as i32;
                if let Err(e) = encoder_guard.set_bitrate(Bitrate::Bits(current_bitrate)) {
                    log_message(&format!("Failed to update bitrate: {:?}", e));
                }
                
                let mut encoded = [0u8; 400];
                match encoder_guard.encode(&pcm, &mut encoded) {
                    Ok(len) => {
                        if len > 0 {
                            match socket_tx.send(&encoded[..len]) {
                                Ok(_) => {},
                                Err(e) => {
                                    log_message(&format!("Send error: {}", e));
                                }
                            }
                        }
                    },
                    Err(e) => {
                        log_message(&format!("Encoding error: {:?}", e));
                    }
                }
            }
        },
        move |err| {
            log_message(&format!("Input stream error: {:?}", err));
        },
        None
    ) {
        Ok(stream) => stream,
        Err(e) => {
            log_message(&format!("Failed to build input stream: {:?}", e));
            return error_codes::INPUT_STREAM_FAILED;
        }
    };
    
    if let Err(e) = input_stream.play() {
        log_message(&format!("Failed to play input stream: {:?}", e));
        return error_codes::INPUT_STREAM_FAILED;
    }
    
    *client.input_stream.lock().unwrap() = Some(input_stream);
    
    // Audio output thread
    let running2 = running.clone();
    let playback_buffer_clone = playback_buffer.clone();
    let output_stream = match output_device.build_output_stream(
        &stream_config,
        move |data: &mut [f32], _: &_| {
            if !running2.load(Ordering::SeqCst) {
                return;
            }
            
            let mut buffer = match playback_buffer_clone.lock() {
                Ok(b) => b,
                Err(_) => return,
            };
            
            for sample in data.iter_mut() {
                *sample = buffer.pop_front().unwrap_or(0.0);
            }
        },
        move |err| {
            log_message(&format!("Output stream error: {:?}", err));
        },
        None
    ) {
        Ok(stream) => stream,
        Err(e) => {
            log_message(&format!("Failed to build output stream: {:?}", e));
            return error_codes::OUTPUT_STREAM_FAILED;
        }
    };
    
    if let Err(e) = output_stream.play() {
        log_message(&format!("Failed to play output stream: {:?}", e));
        return error_codes::OUTPUT_STREAM_FAILED;
    }
    
    *client.output_stream.lock().unwrap() = Some(output_stream);
    
    // Network receiver thread
    let running3 = running.clone();
    thread::spawn(move || {
        log_message("Starting audio receiver thread");
        
        let mut buf = [0u8; MAX_PACKET_SIZE];
        let mut pcm = vec![0i16; FRAME_SIZE];
        let mut decoder = match Decoder::new(SAMPLE_RATE, CHANNELS) {
            Ok(dec) => dec,
            Err(e) => {
                log_message(&format!("Decoder creation error: {:?}", e));
                return;
            }
        };
        
        let mut packet_counter = 0;
        let mut last_receive_time = Instant::now();
        
        while running3.load(Ordering::SeqCst) {
            match socket_rx.recv(&mut buf) {
                Ok(size) => {
                    // Пропускаем keep-alive пакеты
                    if size <= 1 {
                        continue;
                    }
                    
                    if size > 1 {
                        packet_counter += 1;
                        
                        match decoder.decode(&buf[..size], &mut pcm, false) {
                            Ok(samples) => {
                                let receive_time = Instant::now();
                                let delay = receive_time.duration_since(last_receive_time);
                                last_receive_time = receive_time;
                                
                                let samples_f32: Vec<f32> = pcm[..samples]
                                    .iter()
                                    .map(|&s| (s as f32) / 32768.0)
                                    .collect();
                                
                                let mut audio_buf = match playback_buffer.lock() {
                                    Ok(b) => b,
                                    Err(_) => continue,
                                };
                                
                                audio_buf.extend(samples_f32);
                                
                                // Поддержка размера буфера
                                let max_capacity = (SAMPLE_RATE * BUFFER_DURATION_MS / 1000) as usize;
                                while audio_buf.len() > max_capacity {
                                    audio_buf.pop_front();
                                }
                                
                                if packet_counter % 10 == 0 {
                                    let buf_ms = (audio_buf.len() as f32 / SAMPLE_RATE as f32 * 1000.0) as u32;
                                    log_message(&format!(
                                        "Received packet #{}, size: {}b, delay: {:?}, buffer: {}ms",
                                        packet_counter, size, delay, buf_ms
                                    ));
                                }
                            },
                            Err(e) => {
                                log_message(&format!("Decoding error: {:?}", e));
                            }
                        }
                    }
                },
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1));
                },
                Err(e) => {
                    log_message(&format!("Receive error: {}", e));
                }
            }
        }
        
        log_message("Audio receiver thread stopped");
    });
    
    // Keep-alive thread
    let running4 = running.clone();
    let socket_ka = client.socket.clone();
    let is_transmitting_ka = client.is_transmitting.clone();
    thread::spawn(move || {
        log_message("Starting keep-alive thread");
        
        let ka_packet = [0u8; 1];
        let mut ka_counter = 0;
        
        while running4.load(Ordering::SeqCst) {
            thread::sleep(KEEP_ALIVE_INTERVAL);
            if !is_transmitting_ka.load(Ordering::SeqCst) {
                ka_counter += 1;
                match socket_ka.send(&ka_packet) {
                    Ok(_) => {
                        if ka_counter % 10 == 0 {
                            log_message(&format!("Sent keep-alive packet #{} to {}", ka_counter, server_addr));
                        }
                    },
                    Err(e) => {
                        log_message(&format!("Keep-alive send error: {}", e));
                    }
                }
            }
        }
        
        log_message("Keep-alive thread stopped");
    });
    
    log_message("Voice client fully started");
    error_codes::SUCCESS
}

#[no_mangle]
pub extern "C" fn voice_client_stop(client: *mut c_void) {
    if client.is_null() {
        log_message("voice_client_stop: client is null!");
        return;
    }
    
    let client = unsafe { &mut *(client as *mut VoiceClient) };
    
    log_message("Stopping voice client");
    
    client.running.store(false, Ordering::SeqCst);
    
    *client.input_stream.lock().unwrap() = None;
    *client.output_stream.lock().unwrap() = None;
    
    log_message("Voice client stopped");
}

#[no_mangle]
pub extern "C" fn voice_client_set_transmitting(client: *mut c_void, transmitting: bool) {
    if client.is_null() {
        log_message("voice_client_set_transmitting: client is null!");
        return;
    }
    
    let client = unsafe { &mut *(client as *mut VoiceClient) };
    client.is_transmitting.store(transmitting, Ordering::SeqCst);
    
    log_message(&format!("Transmitting: {}", transmitting));
}

#[no_mangle]
pub extern "C" fn voice_client_free(client: *mut c_void) {
    if client.is_null() {
        log_message("voice_client_free: client is null!");
        return;
    }
    
    unsafe { 
        log_message("Freeing voice client");
        voice_client_stop(client);
        let _ = Box::from_raw(client as *mut VoiceClient);
    };
}

#[no_mangle]
pub extern "C" fn voice_client_set_bitrate(client: *mut c_void, bitrate: u32) -> i32 {
    if client.is_null() {
        return error_codes::NULL_POINTER;
    }
    
    let client = unsafe { &mut *(client as *mut VoiceClient) };
    
    if bitrate < 6000 || bitrate > 510000 {
        return error_codes::INVALID_AUDIO_PARAM;
    }
    
    client.bitrate.store(bitrate, Ordering::Relaxed);
    log_message(&format!("Bitrate set to {} bps", bitrate));
    
    if client.running.load(Ordering::SeqCst) {
        if let Ok(mut encoder) = client.encoder.lock() {
            if let Err(e) = encoder.set_bitrate(Bitrate::Bits(bitrate as i32)) {
                log_message(&format!("Failed to set bitrate: {:?}", e));
            }
        }
    }
    
    error_codes::SUCCESS
}