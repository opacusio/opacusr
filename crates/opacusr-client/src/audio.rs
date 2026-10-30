//! Viewer-side audio playback.
//!
//! Encoded Opus packets arrive from the host on the media channel; this
//! module decodes them (48 kHz mono) and plays them through the default
//! output device via `cpal`. A small prebuffer (~4 frames) absorbs network
//! jitter; underruns simply play silence — no attempt at clock recovery,
//! see docs/roadmap.md.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use audiopus::{coder::Decoder, Channels, SampleRate};
use opacusr_proto::audio::{FRAME_SAMPLES, SAMPLE_RATE};

/// Prebuffer in frames (20 ms each) before playback starts / resumes.
const PREBUFFER_FRAMES: usize = 4;

/// A live playback stream. `push()` feeds encoded packets; dropping or
/// [`Self::stop`]ping shuts everything down.
pub struct AudioPlayer {
    tx: std::sync::mpsc::Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AudioPlayer {
    pub fn start() -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let stop2 = stop.clone();
        let thread = std::thread::Builder::new()
            .name("opacusr-audio-play".into())
            .spawn(move || {
                if let Err(e) = play_loop(rx, stop2) {
                    tracing::warn!(target: "opacusr", "audio playback ended: {e}");
                }
            })
            .map_err(|e| format!("audio thread: {e}"))?;
        Ok(Self {
            tx,
            stop,
            thread: Some(thread),
        })
    }

    /// Queue one encoded Opus packet (non-blocking, bounded queue inside).
    pub fn push(&self, packet: Vec<u8>) {
        // 100 packets ≈ 2 s of audio — drop beyond that (network burst).
        let _ = self.tx.send(packet);
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn play_loop(rx: std::sync::mpsc::Receiver<Vec<u8>>, stop: Arc<AtomicBool>) -> Result<(), String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let mut decoder = Decoder::new(SampleRate::Hz48000, Channels::Mono)
        .map_err(|e| format!("opus decoder: {e}"))?;

    // ---- pick the output format -----------------------------------------
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no output device".to_string())?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let out_rate = supported.sample_rate().0;
    let out_channels = supported.channels() as usize;

    // Decoded samples in the OUTPUT format (interleaved f32).
    let queue: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
    let prebuffered = Arc::new(AtomicBool::new(false));

    let stream = {
        let queue = queue.clone();
        let prebuffered = prebuffered.clone();
        let config = cpal::StreamConfig {
            channels: out_channels as u16,
            sample_rate: cpal::SampleRate(out_rate),
            buffer_size: cpal::BufferSize::Default,
        };
        device
            .build_output_stream(
                &config,
                move |out: &mut [f32], _| {
                    let mut q = match queue.try_lock() {
                        Ok(q) => q,
                        Err(_) => {
                            out.fill(0.0);
                            return;
                        }
                    };
                    if !prebuffered.load(Ordering::Relaxed)
                        && q.len() >= out_channels * FRAME_SAMPLES * PREBUFFER_FRAMES
                    {
                        prebuffered.store(true, Ordering::Relaxed);
                    }
                    let needed = out.len();
                    let have = q.len();
                    if prebuffered.load(Ordering::Relaxed) && have >= needed {
                        for (o, s) in out.iter_mut().zip(q.drain(..needed)) {
                            *o = s;
                        }
                    } else {
                        if have > 0 && prebuffered.load(Ordering::Relaxed) {
                            for (o, s) in out.iter_mut().zip(q.drain(..have)) {
                                *o = s;
                            }
                            out[have..].fill(0.0);
                        } else {
                            out.fill(0.0);
                        }
                        // Underrun: require a fresh prebuffer.
                        prebuffered.store(false, Ordering::Relaxed);
                    }
                },
                |e| tracing::debug!(target: "opacusr", "cpal output: {e}"),
                None,
            )
            .map_err(|e| format!("output stream: {e}"))?
    };
    stream.play().map_err(|e| format!("play: {e}"))?;

    // ---- decode loop -----------------------------------------------------
    let mut pcm = vec![0i16; FRAME_SAMPLES * 4];
    while !stop.load(Ordering::Relaxed) {
        let packet = match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(p) => p,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => return Ok(()),
        };
        let got = match decoder.decode(Some(&packet), &mut pcm, false) {
            Ok(n) => n,
            Err(e) => {
                tracing::debug!(target: "opacusr", "opus decode: {e}");
                continue;
            }
        };
        // mono s16 → f32, resampled/expanded to the output format.
        let mono: Vec<f64> = pcm[..got].iter().map(|&s| s as f64 / 32_768.0).collect();
        let mono_48k: Vec<f64> = if out_rate == SAMPLE_RATE {
            mono
        } else {
            linear_resample(&mono, SAMPLE_RATE as usize, out_rate as usize)
        };
        let mut q = queue.lock().unwrap();
        for s in mono_48k {
            for _ in 0..out_channels {
                q.push_back(s as f32);
            }
        }
        // Bound the queue (~2 s) so a paused output does not grow it.
        let max = out_channels * FRAME_SAMPLES * 100;
        while q.len() > max {
            q.pop_front();
        }
    }
    Ok(())
}

/// Linear interpolation resampler (same math as the host pipeline).
fn linear_resample(input: &[f64], from: usize, to: usize) -> Vec<f64> {
    if from == to || input.is_empty() || from == 0 {
        return input.to_vec();
    }
    let step = from as f64 / to as f64;
    let mut out = Vec::with_capacity((input.len() as f64 / step) as usize);
    let mut pos = 0.0f64;
    let prev = input[0];
    while pos < input.len() as f64 {
        let i = pos.floor() as usize;
        let frac = pos - i as f64;
        let s0 = if i == 0 { prev } else { input[i - 1] };
        let s1 = input.get(i).copied().unwrap_or(prev);
        out.push(s0 + (s1 - s0) * frac);
        pos += step;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_length_matches_ratio() {
        let input: Vec<f64> = (0..48_000).map(|i| (i as f64 * 0.01).sin()).collect();
        let out = linear_resample(&input, 48_000, 44_100);
        let expected = 48_000.0 * 44_100.0 / 48_000.0;
        assert!((out.len() as f64 - expected).abs() < 3.0);
    }
}
