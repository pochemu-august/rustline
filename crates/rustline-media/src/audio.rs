//! Cross-platform Audio I/O using CPAL.
//!
//! Handles microphone capture (resampled to 8000 Hz mono 16-bit PCM)
//! and speaker playback (resampled from 8000 Hz to device native sample rate).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::codec::g711::SAMPLES_PER_FRAME;
use crate::rtp::session::AudioBuffer;

/// Active audio engine controlling microphone capture and speaker playback streams.
pub struct AudioEngine {
    stop_tx: Option<std::sync::mpsc::Sender<()>>,
    playback_buf: AudioBuffer,
    pub is_running: bool,
}

impl AudioEngine {
    /// Initialize the audio subsystem and start playback and capture streams on a dedicated OS thread.
    /// `mic_tx`: channel into which 160-sample (20ms, 8kHz) PCM frames will be pushed.
    /// `playback_buf`: buffer from which incoming RTP PCM samples will be played back.
    pub fn new(mic_tx: mpsc::Sender<Vec<i16>>, playback_buf: AudioBuffer) -> Self {
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let p_buf = Arc::clone(&playback_buf);

        std::thread::spawn(move || {
            let host = cpal::default_host();

            // 1. Setup Speaker Output Stream
            let output_stream = match host.default_output_device() {
                Some(device) => {
                    let device_name = device.name().unwrap_or_else(|_| "Default Output".into());
                    info!(device = %device_name, "Initializing audio playback device");
                    match setup_output_stream(&device, p_buf) {
                        Ok(stream) => {
                            if let Err(e) = stream.play() {
                                warn!("Failed to start output stream: {}", e);
                                None
                            } else {
                                Some(stream)
                            }
                        }
                        Err(e) => {
                            warn!("Failed to setup output stream: {}", e);
                            None
                        }
                    }
                }
                None => {
                    warn!("No default audio output device (speakers) found");
                    None
                }
            };

            // 2. Setup Microphone Capture Stream
            let input_stream = match host.default_input_device() {
                Some(device) => {
                    let device_name = device.name().unwrap_or_else(|_| "Default Input".into());
                    info!(device = %device_name, "Initializing audio capture device");
                    match setup_input_stream(&device, mic_tx) {
                        Ok(stream) => {
                            if let Err(e) = stream.play() {
                                warn!("Failed to start input stream: {}", e);
                                None
                            } else {
                                Some(stream)
                            }
                        }
                        Err(e) => {
                            warn!("Failed to setup input stream: {}", e);
                            None
                        }
                    }
                }
                None => {
                    warn!("No default audio input device (microphone) found");
                    None
                }
            };

            // Keep audio streams active until stop signal
            let _ = stop_rx.recv();
            drop(output_stream);
            drop(input_stream);
            debug!("Audio hardware thread terminated");
        });

        Self {
            stop_tx: Some(stop_tx),
            playback_buf,
            is_running: true,
        }
    }

    /// Access the shared playback buffer for received audio.
    pub fn playback_buffer(&self) -> AudioBuffer {
        Arc::clone(&self.playback_buf)
    }

    /// Stop audio hardware streams.
    pub fn stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        self.is_running = false;
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Setup CPAL output stream (Speakers) with 8kHz -> native sample rate upsampling.
fn setup_output_stream(device: &cpal::Device, playback_buf: AudioBuffer) -> anyhow::Result<Stream> {
    let default_config = device.default_output_config()?;
    let sample_rate = default_config.sample_rate().0;
    let channels = default_config.channels() as usize;

    debug!(sample_rate, channels, "Output device config");

    let err_fn = |err| error!("Audio output stream error: {}", err);

    // State for 8kHz upsampling
    let mut current_sample: i16 = 0;
    let mut sample_phase: f32 = 0.0;
    let phase_step = 8000.0 / sample_rate as f32;

    let stream = match default_config.sample_format() {
        SampleFormat::F32 => device.build_output_stream(
            &default_config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut buf_lock = playback_buf.lock().ok();

                for frame in data.chunks_mut(channels) {
                    sample_phase += phase_step;
                    if sample_phase >= 1.0 {
                        sample_phase -= 1.0;
                        if let Some(ref mut q) = buf_lock {
                            current_sample = q.pop_front().unwrap_or(0);
                        } else {
                            current_sample = 0;
                        }
                    }

                    let val = current_sample as f32 / 32768.0;
                    for sample in frame.iter_mut() {
                        *sample = val;
                    }
                }
            },
            err_fn,
            None,
        )?,
        _ => anyhow::bail!("Unsupported sample format for output device"),
    };

    Ok(stream)
}

/// Setup CPAL input stream (Microphone) with native sample rate -> 8kHz downsampling.
fn setup_input_stream(
    device: &cpal::Device,
    mic_tx: mpsc::Sender<Vec<i16>>,
) -> anyhow::Result<Stream> {
    let default_config = device.default_input_config()?;
    let sample_rate = default_config.sample_rate().0;
    let channels = default_config.channels() as usize;

    debug!(sample_rate, channels, "Input device config");

    let err_fn = |err| error!("Audio input stream error: {}", err);

    let mut accumulator: Vec<i16> = Vec::with_capacity(SAMPLES_PER_FRAME);
    let mut sample_phase: f32 = 0.0;
    let phase_step = 8000.0 / sample_rate as f32;

    let stream = match default_config.sample_format() {
        SampleFormat::F32 => device.build_input_stream(
            &default_config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                for frame in data.chunks(channels) {
                    sample_phase += phase_step;
                    if sample_phase >= 1.0 {
                        sample_phase -= 1.0;
                        let mono_f32 = frame[0].clamp(-1.0, 1.0);
                        let pcm16 = (mono_f32 * 32767.0) as i16;
                        accumulator.push(pcm16);

                        if accumulator.len() >= SAMPLES_PER_FRAME {
                            let frame = std::mem::replace(
                                &mut accumulator,
                                Vec::with_capacity(SAMPLES_PER_FRAME),
                            );
                            let _ = mic_tx.try_send(frame);
                        }
                    }
                }
            },
            err_fn,
            None,
        )?,
        _ => anyhow::bail!("Unsupported sample format for input device"),
    };

    Ok(stream)
}

/// Check if audio devices are present on this machine.
pub fn is_audio_available() -> bool {
    let host = cpal::default_host();
    host.default_output_device().is_some() || host.default_input_device().is_some()
}
