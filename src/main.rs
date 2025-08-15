use cpal::{
    traits::{HostTrait, DeviceTrait, StreamTrait},
    StreamConfig, SampleRate, BufferSize,
};
use opus::{Encoder, Decoder, Channels, Application};
use std::{
    net::UdpSocket,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
    collections::VecDeque,
};
use global_hotkey::{
    hotkey::{HotKey, Modifiers, Code},
    GlobalHotKeyManager, GlobalHotKeyEvent, HotKeyState  // Добавлен импорт HotKeyState
};

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: Channels = Channels::Mono;
const FRAME_SIZE: usize = 480;
const BUFFER_DURATION_MS: u32 = 200;
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(1);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("[CLIENT] Initializing high-quality voice chat...");
    
    // 1. Initialize audio devices
    let host = cpal::default_host();
    let input_device = host.default_input_device().ok_or("No input device")?;
    let output_device = host.default_output_device().ok_or("No output device")?;
    
    println!("[AUDIO] Input: {}", input_device.name()?);
    println!("[AUDIO] Output: {}", output_device.name()?);

    // 2. Network setup
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_nonblocking(true)?;
    socket.connect("fiber-gate.ru:8080")?;
    println!("[NET] Connected to server at {}", socket.peer_addr()?);

    // 3. Transmission state
    let is_transmitting = Arc::new(AtomicBool::new(false));

    // 4. Global hotkey setup
    let is_transmitting_kb = Arc::clone(&is_transmitting);
    thread::spawn(move || {
        println!("[CTRL] Hold ALT+` to talk");
        
        // Создаем хоткей Alt+`
        let hotkey = HotKey::new(Some(Modifiers::ALT), Code::Backquote);
        
        // Регистрируем хоткей
        let manager = GlobalHotKeyManager::new().expect("Failed to create hotkey manager");
        manager.register(hotkey).expect("Failed to register hotkey");
        
        // Состояние клавиш
        let mut hotkey_active = false;
        
        // Обрабатываем события
        for event in GlobalHotKeyEvent::receiver() {
            if event.id == hotkey.id() {
                if event.state == HotKeyState::Pressed {
                    if !hotkey_active {
                        hotkey_active = true;
                        is_transmitting_kb.store(true, Ordering::SeqCst);
                        println!("\n[CTRL] TRANSMITTING (Alt+` pressed)");
                    }
                } else { // Released
                    if hotkey_active {
                        hotkey_active = false;
                        is_transmitting_kb.store(false, Ordering::SeqCst);
                        println!("\n[CTRL] SILENT (Alt+` released)");
                    }
                }
            }
        }
    });

    // 5. Keep-alive packets
    let socket_ka = socket.try_clone()?;
    let is_transmitting_ka = Arc::clone(&is_transmitting);
    thread::spawn(move || {
        let ka_packet = [0u8; 1];
        let mut ka_counter = 0;
        
        loop {
            thread::sleep(KEEP_ALIVE_INTERVAL);
            if !is_transmitting_ka.load(Ordering::SeqCst) {
                ka_counter += 1;
                if let Err(e) = socket_ka.send(&ka_packet) {
                    eprintln!("[NET] Keep-alive error: {}", e);
                } else if ka_counter % 10 == 0 {
                    println!("[NET] Sent keep-alive packet #{}", ka_counter);
                }
            }
        }
    });

    // 6. Audio capture and transmission
    let socket_tx = socket.try_clone()?;
    let is_transmitting_tx = Arc::clone(&is_transmitting);
    let mut packet_counter = 0;
    
    // PCM accumulator
    let pcm_accumulator = Arc::new(Mutex::new(Vec::<f32>::new()));
    let pcm_accumulator_cb = Arc::clone(&pcm_accumulator);
    
    // Создаем кодировщик
    let mut encoder = Encoder::new(SAMPLE_RATE, CHANNELS, Application::Audio)?;
    encoder.set_bitrate(opus::Bitrate::Bits(64000))?;
    let encoder = Arc::new(Mutex::new(encoder));
    let encoder_cb = Arc::clone(&encoder);
    
    let input_stream = input_device.build_input_stream(
        &StreamConfig {
            channels: 1,
            sample_rate: SampleRate(SAMPLE_RATE),
            buffer_size: BufferSize::Default,
        },
        move |data: &[f32], _| {
            if !is_transmitting_tx.load(Ordering::SeqCst) {
                return;
            }
            
            let mut acc = pcm_accumulator_cb.lock().unwrap();
            acc.extend_from_slice(data);
            
            // Process full frames
            while acc.len() >= FRAME_SIZE {
                // Take a frame
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

                // Кодируем
                let mut encoder_guard = encoder_cb.lock().unwrap();
                let mut encoded = vec![0u8; 400];
                if let Ok(len) = encoder_guard.encode(&pcm, &mut encoded) {
                    packet_counter += 1;
                    if let Err(e) = socket_tx.send(&encoded[..len]) {
                        eprintln!("[NET] Send error: {}", e);
                    }
                } else {
                    eprintln!("[AUDIO] Encoding error");
                }
            }
        },
        |err| eprintln!("[AUDIO] Input error: {:?}", err),
        None,
    )?;
    input_stream.play()?;
    println!("[AUDIO] Input stream started");

    // 7. Audio playback
    let buffer_capacity = (SAMPLE_RATE * BUFFER_DURATION_MS / 1000) as usize;
    let playback_buffer = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(buffer_capacity)));
    let playback_buffer_out = Arc::clone(&playback_buffer);
    
    let output_stream = output_device.build_output_stream(
        &StreamConfig {
            channels: 1,
            sample_rate: SampleRate(SAMPLE_RATE),
            buffer_size: BufferSize::Default,
        },
        move |data: &mut [f32], _| {
            let mut buf = playback_buffer_out.lock().unwrap();
            
            for sample in data.iter_mut() {
                *sample = buf.pop_front().unwrap_or(0.0);
            }
        },
        |err| eprintln!("[AUDIO] Output error: {:?}", err),
        None,
    )?;
    output_stream.play()?;
    println!("[AUDIO] Output stream started");

    // 8. Receive audio from server
    let playback_buffer_rx = Arc::clone(&playback_buffer);
    thread::spawn(move || {
        let mut buf = [0u8; 400];
        let mut pcm = vec![0i16; FRAME_SIZE];
        let mut packet_counter = 0;
        let mut last_receive_time = Instant::now();
        
        // Создаем декодер
        let mut decoder = Decoder::new(SAMPLE_RATE, CHANNELS).expect("Failed to create decoder");
        println!("[AUDIO] Decoder initialized");
        
        loop {
            match socket.recv(&mut buf) {
                Ok(size) => {
                    packet_counter += 1;
                    
                    // Ignore keep-alive packets
                    if size > 1 {
                        let receive_time = Instant::now();
                        let delay = receive_time.duration_since(last_receive_time);
                        last_receive_time = receive_time;
                        
                        match decoder.decode(&buf[..size], &mut pcm, false) {
                            Ok(samples) => {
                                // Convert to float
                                let samples_f32: Vec<f32> = pcm[..samples]
                                    .iter()
                                    .map(|&s| (s as f32) / 32768.0)
                                    .collect();
                                
                                // Add to playback buffer
                                let mut audio_buf = playback_buffer_rx.lock().unwrap();
                                audio_buf.extend(&samples_f32);
                                
                                // Log periodically
                                if packet_counter % 50 == 0 {
                                    let buf_ms = (audio_buf.len() as f32 / SAMPLE_RATE as f32 * 1000.0) as u32;
                                    println!("[AUDIO RX] Pkt #{} ({}b) | Delay: {:?} | Buffer: {}ms",
                                        packet_counter, size, delay, buf_ms);
                                }
                            },
                            Err(e) => eprintln!("[AUDIO] Decoding error: {}", e),
                        }
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1));
                },
                Err(e) => eprintln!("[NET] Receive error: {}", e),
            }
        }
    });

    // 9. Initial buffering
    println!("[STATUS] Buffering audio...");
    thread::sleep(Duration::from_millis(500));
    
    println!("[STATUS] Client ready. Hold ALT+` to talk.");

    // 10. Main loop
    loop {
        thread::sleep(Duration::from_secs(5));
        let status = if is_transmitting.load(Ordering::SeqCst) {
            "TRANSMITTING"
        } else {
            "SILENT"
        };
        let buf = playback_buffer.lock().unwrap();
        let buf_ms = (buf.len() as f32 / SAMPLE_RATE as f32 * 1000.0) as u32;
        println!("[STATUS] {} | Buffer: {}ms", status, buf_ms);
    }
}