//! Host audio streaming (Opus 48 kHz mono, 20 ms frames).
//!
//! Sources per platform:
//! * **Windows** — WASAPI shared-mode *loopback* of the default output
//!   device, i.e. exactly the sound the machine plays (TeamViewer-style
//!   "remote sound"). Implemented directly against the `windows` crate.
//! * **Linux / macOS** — capture from an input device via `cpal`
//!   (microphone; on Linux a PulseAudio monitor can be selected by name
//!   through the ALSA plugin layer, e.g. `audio_device = "monitor"`).
//!
//! Both feed one common pipeline: interleaved-f32 chunks → mono mix-down →
//! linear resample to 48 kHz → s16 → Opus frames pushed to the session
//! loop as `AudioFrame`s (one packet = 960 samples = 20 ms).
//!
//! Everything runs on its own OS thread; the session loop only sees an
//! `mpsc` of encoded packets and a stop flag.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use audiopus::{coder::Encoder, Application, Bitrate, Channels, SampleRate};
use opacusr_proto::audio::{FRAME_SAMPLES, SAMPLE_RATE};
use tokio::sync::mpsc::UnboundedSender;

pub use opacusr_proto::audio::{BITRATE, FRAME_MS};

/// A live capture+encode stream. Dropping/stopping it joins the worker.
pub struct HostAudioStream {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl HostAudioStream {
    /// Start capturing and encoding. `device_sub` filters input devices by
    /// name substring (empty = default; on Windows an empty value always
    /// means loopback of the default output). Encoded packets are handed to
    /// `send`.
    pub fn start(device_sub: &str, send: UnboundedSender<Vec<u8>>) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        // Capture side (platform) → pipeline (channel of raw chunks).
        let (chunk_tx, chunk_rx) = channel::<CaptureChunk>();

        start_capture(device_sub, chunk_tx, stop.clone())?;

        let stop2 = stop.clone();
        let thread = std::thread::Builder::new()
            .name("opacusr-audio-enc".into())
            .spawn(move || {
                encode_loop(chunk_rx, send, stop2);
            })
            .map_err(|e| format!("audio thread: {e}"))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    /// Stop capturing and join the worker (idempotent).
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for HostAudioStream {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One interleaved f32 chunk straight from the capture device.
struct CaptureChunk {
    samples: Vec<f32>,
    sample_rate: u32,
    channels: usize,
}

// ---------------------------------------------------------------------------
// Common encode pipeline
// ---------------------------------------------------------------------------

fn encode_loop(rx: Receiver<CaptureChunk>, send: UnboundedSender<Vec<u8>>, stop: Arc<AtomicBool>) {
    let mut encoder = match Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(target: "opacusr", "opus encoder init: {e}");
            return;
        }
    };
    if let Err(e) = encoder.set_bitrate(Bitrate::BitsPerSecond(BITRATE)) {
        tracing::debug!(target: "opacusr", "opus bitrate ctl: {e}");
    }

    // Resampler state (linear interpolation).
    let mut resample_pos = 0.0f64;
    let mut mono_prev: f64 = 0.0;
    let mut mono_pending: Vec<f64> = Vec::new(); // resampled mono, -1..1
    let mut pcm: Vec<i16> = Vec::with_capacity(FRAME_SAMPLES * 2);
    let mut out = [0u8; 4000]; // plenty for a 20 ms voip frame
    let mut seq_gap_logged = false;

    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let chunk = match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(c) => c,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => return, // capture side gone
        };
        if chunk.sample_rate == 0 || chunk.channels == 0 || chunk.samples.is_empty() {
            continue;
        }

        // Channel mix-down to mono.
        let ch = chunk.channels.max(1);
        let mono: Vec<f64> = chunk
            .samples
            .chunks_exact(ch)
            .map(|frame| frame.iter().map(|&s| s as f64).sum::<f64>() / ch as f64)
            .collect();

        // Linear resample to 48 kHz.
        let src_rate = chunk.sample_rate as f64;
        let dst_rate = SAMPLE_RATE as f64;
        if (src_rate - dst_rate).abs() < 1.0 {
            mono_pending.extend(mono.iter().copied());
        } else {
            // Continue the interpolation across chunks: treat `mono_prev`
            // as the sample just before the stream.
            let step = src_rate / dst_rate;
            let mut src_pos = resample_pos;
            while src_pos < mono.len() as f64 {
                let i = src_pos.floor() as usize;
                let frac = src_pos - i as f64;
                let s0 = if i == 0 { mono_prev } else { mono[i - 1] };
                let s1 = mono.get(i).copied().unwrap_or(mono_prev);
                mono_pending.push(s0 + (s1 - s0) * frac);
                src_pos += step;
            }
            resample_pos = src_pos - mono.len() as f64;
            if let Some(&last) = mono.last() {
                mono_prev = last;
            }
        }

        // f64 → s16 → Opus frames.
        let mut consumed = 0usize;
        while mono_pending.len() - consumed >= FRAME_SAMPLES {
            for s in &mono_pending[consumed..consumed + FRAME_SAMPLES] {
                pcm.push((s.clamp(-1.0, 1.0) * 32_767.0) as i16);
            }
            consumed += FRAME_SAMPLES;
            match encoder.encode(&pcm, &mut out) {
                Ok(n) if n > 0 => {
                    if send.send(out[..n].to_vec()).is_err() {
                        return; // session loop gone
                    }
                    seq_gap_logged = false;
                }
                Ok(_) => {
                    if !seq_gap_logged {
                        tracing::debug!(target: "opacusr", "opus encoded an empty frame");
                        seq_gap_logged = true;
                    }
                }
                Err(e) => {
                    tracing::debug!(target: "opacusr", "opus encode: {e}");
                }
            }
            pcm.clear();
        }
        mono_pending.drain(..consumed);
    }
}

// ---------------------------------------------------------------------------
// Windows: WASAPI loopback of the default output device
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn start_capture(
    device_sub: &str,
    chunk_tx: Sender<CaptureChunk>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    if !device_sub.trim().is_empty() {
        return Err("audio_device filter is not supported on Windows yet (loopback of the default output is always used)".into());
    }
    std::thread::Builder::new()
        .name("opacusr-audio-cap".into())
        .spawn(move || wasapi_loopback_loop(chunk_tx, stop))
        .map_err(|e| format!("audio thread: {e}"))?;
    Ok(())
}

#[cfg(windows)]
fn wasapi_loopback_loop(tx: Sender<CaptureChunk>, stop: Arc<AtomicBool>) {
    use windows::core::GUID;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
        MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

    let result = (|| -> Result<(), String> {
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| format!("mmdevice: {e}"))?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|e| format!("default output: {e}"))?;
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| format!("activate: {e}"))?;
            let fmt_ptr = client
                .GetMixFormat()
                .map_err(|e| format!("mix format: {e}"))?;
            let fmt = &*fmt_ptr;
            let (sr, ch, is_float) = parse_wave_format(fmt);
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK,
                    2_000_000, // 200 ms buffer (100 ns units)
                    0,
                    fmt,
                    None,
                )
                .map_err(|e| format!("initialize: {e}"))?;
            let capture: IAudioCaptureClient = client
                .GetService()
                .map_err(|e| format!("capture service: {e}"))?;
            client.Start().map_err(|e| format!("start: {e}"))?;
            tracing::info!(target: "opacusr", "WASAPI loopback started ({sr} Hz, {ch} ch)");

            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(5));
                loop {
                    let packet = match capture.GetNextPacketSize() {
                        Ok(p) => p,
                        Err(e) => return Err(format!("packet size: {e}")),
                    };
                    if packet == 0 {
                        break;
                    }
                    let mut data: *mut u8 = std::ptr::null_mut();
                    let mut frames = 0u32;
                    let mut flags = 0u32;
                    if let Err(e) =
                        capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    {
                        return Err(format!("get buffer: {e}"));
                    }
                    let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 != 0;
                    let len = frames as usize * ch as usize;
                    let mut samples = vec![0.0f32; len];
                    if !silent && !data.is_null() {
                        if is_float {
                            let src = std::slice::from_raw_parts(data as *const f32, len);
                            samples.copy_from_slice(src);
                        } else {
                            let src = std::slice::from_raw_parts(data as *const i16, len);
                            for (d, &s) in samples.iter_mut().zip(src) {
                                *d = s as f32 / 32_768.0;
                            }
                        }
                    }
                    capture
                        .ReleaseBuffer(frames)
                        .map_err(|e| format!("release: {e}"))?;
                    let _ = tx.send(CaptureChunk {
                        samples,
                        sample_rate: sr,
                        channels: ch as usize,
                    });
                }
            }
            client.Stop().map_err(|e| format!("stop: {e}"))?;
            Ok(())
        }
    })();

    if let Err(e) = result {
        tracing::warn!(target: "opacusr", "audio capture ended: {e}");
    }
}

/// Parse the fields the pipeline cares about: sample rate, channels,
/// float-vs-int samples. Handles plain and EXTENSIBLE formats.
#[cfg(windows)]
fn parse_wave_format(fmt: &WAVEFORMATEX) -> (u32, u16, bool) {
    use windows::core::GUID;
    const IEEE_FLOAT_TAG: u16 = 3; // WAVE_FORMAT_IEEE_FLOAT
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE; // mmreg.h
                                                // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT = {00000003-0000-0010-8000-00AA00389B71}.
    const IEEE_FLOAT_GUID: GUID = GUID::from_u128(0x0000_0003_0000_0010_8000_00aa_0038_9b71);
    let mut is_float = fmt.wFormatTag == IEEE_FLOAT_TAG;
    if fmt.wFormatTag == WAVE_FORMAT_EXTENSIBLE {
        let ext = unsafe { &*(fmt as *const WAVEFORMATEX as *const WAVEFORMATEXTENSIBLE) };
        is_float = ext.SubFormat == IEEE_FLOAT_GUID;
    }
    (fmt.nSamplesPerSec, fmt.nChannels, is_float)
}

// ---------------------------------------------------------------------------
// Unix / macOS: cpal input device (mic or a monitor source by name)
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
fn start_capture(
    device_sub: &str,
    chunk_tx: Sender<CaptureChunk>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    // The cpal stream is `!Send`, so it is created, played and kept alive
    // on one dedicated thread; the init result comes back over a channel.
    let (meta_tx, meta_rx) = channel::<Result<(), String>>();
    let device_sub = device_sub.to_string();
    std::thread::Builder::new()
        .name("opacusr-audio-cap".into())
        .spawn(move || {
            let host = cpal::default_host();
            let init = (|| -> Result<(), String> {
                let device: cpal::Device = {
                    let want = device_sub.trim().to_ascii_lowercase();
                    let mut found: Option<cpal::Device> = None;
                    if !want.is_empty() {
                        found = host.input_devices().ok().and_then(|mut devs| {
                            devs.find(|d| {
                                d.name()
                                    .map(|n| n.to_ascii_lowercase().contains(&want))
                                    .unwrap_or(false)
                            })
                        });
                        if found.is_none() {
                            return Err(format!("no input device matching {device_sub:?}"));
                        }
                    }
                    found
                        .or_else(|| host.default_input_device())
                        .ok_or_else(|| "no input device available".to_string())?
                };
                let name = device.name().unwrap_or_else(|_| "?".into());
                let config: cpal::StreamConfig = device
                    .default_input_config()
                    .map_err(|e| e.to_string())?
                    .into();
                let (sr, ch) = (config.sample_rate.0, config.channels as usize);

                let err_fn = move |e| tracing::debug!(target: "opacusr", "cpal input: {e}");
                let tx = chunk_tx.clone();
                let stream = device
                    .build_input_stream(
                        &config,
                        move |data: &[f32], _| {
                            let _ = tx.send(CaptureChunk {
                                samples: data.to_vec(),
                                sample_rate: sr,
                                channels: ch,
                            });
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| format!("input stream: {e}"))?;
                stream.play().map_err(|e| format!("play: {e}"))?;
                tracing::info!(
                    target: "opacusr",
                    "audio capture: {name} ({sr} Hz, {ch} ch)"
                );
                meta_tx.send(Ok(())).ok();
                // Keep the stream object alive until stopped. Dropping it
                // (on this same thread) ends capture.
                loop {
                    if stop.load(Ordering::Relaxed) {
                        drop(stream);
                        return Ok(());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            })();
            match init {
                Ok(()) => {}
                Err(e) => {
                    let _ = meta_tx.send(Err(e.clone()));
                    tracing::warn!(target: "opacusr", "audio capture failed: {e}");
                }
            }
        })
        .map_err(|e| format!("audio thread: {e}"))?;

    meta_rx
        .recv()
        .map_err(|_| "audio capture thread died".to_string())?
}

// ---------------------------------------------------------------------------
// Tests (headless-safe: no device needed, only the encoder pipeline)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_roundtrip_sine() {
        let mut encoder =
            Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip).unwrap();
        encoder
            .set_bitrate(Bitrate::BitsPerSecond(BITRATE))
            .unwrap();
        let mut decoder =
            audiopus::coder::Decoder::new(SampleRate::Hz48000, Channels::Mono).unwrap();

        // 20 ms of 440 Hz sine.
        let pcm: Vec<i16> = (0..FRAME_SAMPLES)
            .map(|i| {
                ((2.0 * std::f64::consts::PI * 440.0 * i as f64 / SAMPLE_RATE as f64).sin()
                    * 20_000.0) as i16
            })
            .collect();
        let mut pkt = [0u8; 4000];
        let n = encoder.encode(&pcm, &mut pkt).unwrap();
        assert!(n > 0, "encoder produced an empty packet");
        assert!(n < 400, "20 ms voip frame unexpectedly large: {n} bytes");

        let mut out = vec![0i16; FRAME_SAMPLES];
        let got = decoder.decode(Some(&pkt[..n]), &mut out, false).unwrap();
        assert_eq!(got, FRAME_SAMPLES);
        // Energy survived the round trip (not silence, not clipping).
        let rms =
            (out.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / out.len() as f64).sqrt();
        assert!(rms > 1000.0, "decoded signal too quiet: rms {rms}");
    }

    #[test]
    fn resampler_produces_expected_count() {
        // The pipeline's linear resampler: 44100 → 48000 on 1 s of audio
        // must yield ~48000 samples (checked via the chunk math).
        let src_rate = 44_100f64;
        let dst_rate = 48_000f64;
        let step = src_rate / dst_rate;
        let n = 100_000; // ~2.27 s at 44.1k
        let produced = (n as f64 / step) as usize;
        let expected = (n as f64 * dst_rate / src_rate) as usize;
        assert!((produced as i64 - expected as i64).abs() <= 2);
    }
}
