//! Real audio hardware device driver (Microphone capture & Speaker playback via `cpal`).
//!
//! Bridges RTP 8000 Hz G.711u audio with physical OS sound devices (WASAPI on Windows,
//! ALSA/PulseAudio/PipeWire on Linux, CoreAudio on macOS).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};
use tracing::{info, warn};

use crate::audio::g711::{linear_to_ulaw, ulaw_to_linear};

/// Audio handle managing active microphone and speaker streams for a call.
pub struct AudioDeviceSession {
    _input_stream: Option<Stream>,
    _output_stream: Option<Stream>,
    mic_buffer: Arc<Mutex<Vec<u8>>>,
    speaker_buffer: Arc<Mutex<Vec<i16>>>,
    stopped: Arc<AtomicBool>,
}

unsafe impl Send for AudioDeviceSession {}
unsafe impl Sync for AudioDeviceSession {}

impl AudioDeviceSession {
    /// Starts capturing microphone and playing to speakers for a call.
    pub fn start() -> Result<Self, String> {
        let host = cpal::default_host();
        let stopped = Arc::new(AtomicBool::new(false));

        let mic_buffer = Arc::new(Mutex::new(Vec::with_capacity(3200)));
        let speaker_buffer = Arc::new(Mutex::new(Vec::with_capacity(3200)));

        // ── 1. Setup Speaker Playback ──────────────────────────────────────────
        let output_stream = match host.default_output_device() {
            Some(device) => {
                let name = device.name().unwrap_or_else(|_| "Default Speaker".to_string());
                info!(device = %name, "opening physical audio output (speakers)");

                match device.default_output_config() {
                    Ok(config) => {
                        let sample_rate = config.sample_rate().0;
                        let channels = config.channels() as usize;
                        let sample_format = config.sample_format();
                        let spk_buf = Arc::clone(&speaker_buffer);
                        let is_stopped = Arc::clone(&stopped);

                        let stream_res = match sample_format {
                            SampleFormat::F32 => {
                                device.build_output_stream(
                                    &config.into(),
                                    move |data: &mut [f32], _| {
                                        if is_stopped.load(Ordering::Relaxed) {
                                            data.fill(0.0);
                                            return;
                                        }
                                        let mut buf = spk_buf.lock().unwrap();
                                        // Resample 8000 Hz mono -> device sample_rate (e.g. 48000 Hz) stereo/mono
                                        let _step = 8000.0 / sample_rate as f32;
                                        for frame in data.chunks_mut(channels) {
                                            let sample_val = if !buf.is_empty() {
                                                buf.remove(0) as f32 / 32768.0
                                            } else {
                                                0.0
                                            };
                                            // Duplicate sample to all channels (stereo/mono)
                                            for ch in frame.iter_mut() {
                                                *ch = sample_val;
                                            }
                                        }
                                    },
                                    |err| warn!("audio output stream error: {err}"),
                                    None,
                                )
                            }
                            SampleFormat::I16 => {
                                device.build_output_stream(
                                    &config.into(),
                                    move |data: &mut [i16], _| {
                                        if is_stopped.load(Ordering::Relaxed) {
                                            data.fill(0);
                                            return;
                                        }
                                        let mut buf = spk_buf.lock().unwrap();
                                        for frame in data.chunks_mut(channels) {
                                            let sample_val = if !buf.is_empty() {
                                                buf.remove(0)
                                            } else {
                                                0
                                            };
                                            for ch in frame.iter_mut() {
                                                *ch = sample_val;
                                            }
                                        }
                                    },
                                    |err| warn!("audio output stream error: {err}"),
                                    None,
                                )
                            }
                            _ => Err(cpal::BuildStreamError::DeviceNotAvailable),
                        };

                        match stream_res {
                            Ok(stream) => {
                                if let Err(e) = stream.play() {
                                    warn!("failed to start audio playback stream: {e}");
                                    None
                                } else {
                                    Some(stream)
                                }
                            }
                            Err(e) => {
                                warn!("failed to build output stream: {e}");
                                None
                            }
                        }
                    }
                    Err(e) => {
                        warn!("failed to get default output config: {e}");
                        None
                    }
                }
            }
            None => {
                warn!("no default audio output device (speakers) found");
                None
            }
        };

        // ── 2. Setup Microphone Capture ─────────────────────────────────────────
        let input_stream = match host.default_input_device() {
            Some(device) => {
                let name = device.name().unwrap_or_else(|_| "Default Mic".to_string());
                info!(device = %name, "opening physical audio input (microphone)");

                match device.default_input_config() {
                    Ok(config) => {
                        let sample_rate = config.sample_rate().0;
                        let channels = config.channels() as usize;
                        let sample_format = config.sample_format();
                        let mic_buf = Arc::clone(&mic_buffer);
                        let is_stopped = Arc::clone(&stopped);

                        // Simple decimation ratio: e.g. 48000 / 8000 = 6
                        let decimation = (sample_rate / 8000).max(1) as usize;

                        let stream_res = match sample_format {
                            SampleFormat::F32 => {
                                let mut sample_counter: usize = 0;
                                device.build_input_stream(
                                    &config.into(),
                                    move |data: &[f32], _| {
                                        if is_stopped.load(Ordering::Relaxed) {
                                            return;
                                        }
                                        let mut encoded_samples = Vec::new();
                                        for frame in data.chunks(channels) {
                                            sample_counter += 1;
                                            if sample_counter >= decimation {
                                                sample_counter = 0;
                                                // Average channels to mono
                                                let mono_sample: f32 = frame.iter().sum::<f32>() / channels as f32;
                                                let pcm = (mono_sample.clamp(-1.0, 1.0) * 32767.0) as i16;
                                                encoded_samples.push(linear_to_ulaw(pcm));
                                            }
                                        }
                                        if !encoded_samples.is_empty() {
                                            let mut buf = mic_buf.lock().unwrap();
                                            // Limit buffer size to ~1 second to prevent latency build up
                                            if buf.len() < 8000 {
                                                buf.extend_from_slice(&encoded_samples);
                                            }
                                        }
                                    },
                                    |err| warn!("audio input stream error: {err}"),
                                    None,
                                )
                            }
                            SampleFormat::I16 => {
                                let mut sample_counter: usize = 0;
                                device.build_input_stream(
                                    &config.into(),
                                    move |data: &[i16], _| {
                                        if is_stopped.load(Ordering::Relaxed) {
                                            return;
                                        }
                                        let mut encoded_samples = Vec::new();
                                        for frame in data.chunks(channels) {
                                            sample_counter += 1;
                                            if sample_counter >= decimation {
                                                sample_counter = 0;
                                                let mono_sample: i32 = frame.iter().map(|&s| s as i32).sum::<i32>() / channels as i32;
                                                let pcm = mono_sample.clamp(-32768, 32767) as i16;
                                                encoded_samples.push(linear_to_ulaw(pcm));
                                            }
                                        }
                                        if !encoded_samples.is_empty() {
                                            let mut buf = mic_buf.lock().unwrap();
                                            if buf.len() < 8000 {
                                                buf.extend_from_slice(&encoded_samples);
                                            }
                                        }
                                    },
                                    |err| warn!("audio input stream error: {err}"),
                                    None,
                                )
                            }
                            _ => Err(cpal::BuildStreamError::DeviceNotAvailable),
                        };

                        match stream_res {
                            Ok(stream) => {
                                if let Err(e) = stream.play() {
                                    warn!("failed to start audio input stream: {e}");
                                    None
                                } else {
                                    Some(stream)
                                }
                            }
                            Err(e) => {
                                warn!("failed to build input stream: {e}");
                                None
                            }
                        }
                    }
                    Err(e) => {
                        warn!("failed to get default input config: {e}");
                        None
                    }
                }
            }
            None => {
                warn!("no default audio input device (microphone) found");
                None
            }
        };

        Ok(Self {
            _input_stream: input_stream,
            _output_stream: output_stream,
            mic_buffer,
            speaker_buffer,
            stopped,
        })
    }

    /// Pulls 160 bytes (20ms @ 8000 Hz) of encoded G.711u audio from the microphone.
    /// If not enough samples, returns digital silence (0xFF).
    pub fn read_mic_frame(&self) -> Vec<u8> {
        let mut buf = self.mic_buffer.lock().unwrap();
        if buf.len() >= 160 {
            buf.drain(0..160).collect()
        } else {
            // Silence in G.711u is 0xFF
            vec![0xFF; 160]
        }
    }

    /// Feeds incoming G.711u payload (160 bytes) from RTP into the speaker playback buffer.
    pub fn write_speaker_frame(&self, ulaw_payload: &[u8]) {
        let mut pcm_samples = Vec::with_capacity(ulaw_payload.len() * 6);
        for &byte in ulaw_payload {
            let pcm = ulaw_to_linear(byte);
            // Upsample 8000 Hz -> 48000 Hz by repeating sample 6 times
            for _ in 0..6 {
                pcm_samples.push(pcm);
            }
        }
        let mut buf = self.speaker_buffer.lock().unwrap();
        // Limit playback buffer to ~500ms to avoid drift
        if buf.len() < 24000 {
            buf.extend_from_slice(&pcm_samples);
        }
    }

    /// Stops audio capture and playback.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}
