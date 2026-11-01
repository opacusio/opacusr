//! Opt-in host-side session recording: encoded H.264 frames → MP4 file.
//!
//! The recorder taps the wire-ready Annex-B bitstream right after the
//! encoder (zero transcoding): NAL units are re-packaged into AVCC
//! (length-prefixed) samples and muxed into a standard MP4 with an
//! `avcC` record built from the encoder's SPS/PPS. Recording starts at
//! the first keyframe (everything before it is dropped — a valid H.264
//! stream must begin with an IDR anyway) and rotates to a new `-partN`
//! file when the stream resolution changes (monitor switch).
//!
//! Fail-open by design: every recorder error poisons the recorder and is
//! reported to the caller; the session itself never breaks because of it.

use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::Instant;

use mp4::{
    AvcConfig, FourCC, MediaConfig, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig, TrackType,
};

/// MP4 video timescale (90 kHz — the H.264 convention).
const TIMESCALE: u32 = 90_000;
/// Frames buffered while waiting for the encoder's SPS/PPS (first IDR).
const PROBE_LIMIT: usize = 32;
/// Upper bound for one sample's duration (capture pump stalled / paused).
const MAX_DT_MS: u64 = 1000;

/// One host session's video recording. Created per session via
/// [`SessionRecorder::start`], fed encoded frames via [`SessionRecorder::write`],
/// finalized with [`SessionRecorder::finish`] (idempotent).
pub struct SessionRecorder {
    dir: PathBuf,
    stem: String,
    /// File rotation counter (resolution changes → `-partN` files).
    part: u32,
    writer: Option<Mp4Writer<BufWriter<std::fs::File>>>,
    /// Stream resolution the current file was opened with.
    size: (u32, u32),
    configured: bool,
    /// True once an error closed the recording (never writes again).
    poisoned: bool,
    /// True after [`SessionRecorder::finish`] (never writes again).
    closed: bool,
    last: Option<Instant>,
    /// Write position of the next sample, in `TIMESCALE` units.
    next_start: u64,
    /// Frames held while waiting for the first SPS/PPS.
    probe: Vec<Vec<u8>>,
    /// Duration assigned to the first sample (from `fps_hint`).
    first_dur_ms: u64,
    last_error: Option<String>,
}

impl SessionRecorder {
    /// Prepare a recording `dir/<timestamp>-<peer>.mp4`. The MP4 file is
    /// created lazily on the first keyframe (SPS/PPS needed for `avcC`).
    pub fn start(dir: &Path, peer: &str, fps_hint: u8) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let stem = format!("{}-{}", timestamp_stem(), sanitize(peer));
        Ok(Self {
            dir: dir.to_path_buf(),
            stem,
            part: 0,
            writer: None,
            size: (0, 0),
            configured: false,
            poisoned: false,
            closed: false,
            last: None,
            next_start: 0,
            probe: Vec::new(),
            first_dur_ms: (1000 / u64::from(fps_hint.max(1))).clamp(16, 100),
            last_error: None,
        })
    }

    /// Path of the file currently being written (or about to be).
    pub fn path(&self) -> PathBuf {
        self.file_path(self.part)
    }

    /// Why the recording stopped, if it did.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Record one encoded H.264 frame (Annex-B bitstream, as produced by
    /// the encoder and sent on the wire). Returns `false` when the
    /// recording stopped — the caller should read [`SessionRecorder::last_error`]
    /// and drop the recorder; the session keeps streaming either way.
    pub fn write(&mut self, at: Instant, annex_b: &[u8], w: u32, h: u32) -> bool {
        if self.poisoned || self.closed {
            return true;
        }
        if let Err(e) = self.write_inner(at, annex_b, w, h) {
            self.poisoned = true;
            self.last_error = Some(e);
            let _ = self.finish();
            return false;
        }
        true
    }

    /// Finalize the MP4 (write the `moov` atom). Idempotent; safe to call
    /// on a poisoned recorder.
    pub fn finish(&mut self) -> std::io::Result<()> {
        self.closed = true;
        if let Some(mut w) = self.writer.take() {
            w.write_end().map_err(std::io::Error::other)?;
        }
        Ok(())
    }

    fn file_path(&self, part: u32) -> PathBuf {
        if part == 0 {
            self.dir.join(format!("{}.mp4", self.stem))
        } else {
            self.dir.join(format!("{}-part{part}.mp4", self.stem))
        }
    }

    fn write_inner(&mut self, at: Instant, annex_b: &[u8], w: u32, h: u32) -> Result<(), String> {
        if annex_b.is_empty() {
            return Ok(());
        }
        if self.configured && self.size != (w, h) {
            // Resolution changed (monitor switch): finalize this file and
            // rotate into a new `-partN` file for the new geometry.
            self.finish().map_err(|e| format!("mp4 finalize: {e}"))?;
            self.configured = false;
            self.part += 1;
            self.next_start = 0;
            self.last = None;
            self.probe.clear();
        }
        let nals = split_nals(annex_b);
        if !self.configured {
            let sps = nals.iter().copied().find(|n| nal_type(n) == Some(7));
            let pps = nals.iter().copied().find(|n| nal_type(n) == Some(8));
            let (Some(sps), Some(pps)) = (sps, pps) else {
                // Not a keyframe yet — hold the frame and wait. A valid
                // stream must start from an IDR, so this is transient.
                self.probe.push(annex_b.to_vec());
                if self.probe.len() > PROBE_LIMIT {
                    return Err("no H.264 keyframe received".into());
                }
                return Ok(());
            };
            self.open_track(w, h, sps.to_vec(), pps.to_vec())?;
            self.configured = true;
            self.size = (w, h);
            self.probe.clear(); // pre-IDR frames are not muxed (undecodable)
        }
        let dt_ms = match self.last {
            None => self.first_dur_ms,
            Some(t) => (at.duration_since(t).as_millis() as u64).clamp(1, MAX_DT_MS),
        };
        self.last = Some(at);
        self.push_sample(annex_b, dt_ms)
    }

    fn open_track(&mut self, w: u32, h: u32, sps: Vec<u8>, pps: Vec<u8>) -> Result<(), String> {
        let path = self.file_path(self.part);
        let file =
            std::fs::File::create(&path).map_err(|e| format!("create {}: {e}", path.display()))?;
        let writer = Mp4Writer::write_start(
            BufWriter::new(file),
            &Mp4Config {
                major_brand: "isom".parse::<FourCC>().unwrap(),
                minor_version: 0,
                compatible_brands: vec![
                    "isom".parse::<FourCC>().unwrap(),
                    "mp42".parse::<FourCC>().unwrap(),
                    "avc1".parse::<FourCC>().unwrap(),
                ],
                timescale: TIMESCALE,
            },
        )
        .map_err(|e| format!("mp4 start: {e}"))?;
        let mut writer = writer;
        writer
            .add_track(&TrackConfig {
                track_type: TrackType::Video,
                timescale: TIMESCALE,
                language: "und".into(),
                media_conf: MediaConfig::AvcConfig(AvcConfig {
                    width: w as u16,
                    height: h as u16,
                    seq_param_set: sps,
                    pic_param_set: pps,
                }),
            })
            .map_err(|e| format!("mp4 track: {e}"))?;
        self.writer = Some(writer);
        Ok(())
    }

    fn push_sample(&mut self, annex_b: &[u8], dt_ms: u64) -> Result<(), String> {
        let nals = split_nals(annex_b);
        let bytes = to_avcc(&nals);
        if bytes.is_empty() {
            return Ok(()); // parameter-set-only blob: lives in avcC
        }
        let is_sync = nals.iter().any(|n| nal_type(n) == Some(5));
        let dur = ((dt_ms * u64::from(TIMESCALE)) / 1000).max(1) as u32;
        let sample = Mp4Sample {
            start_time: self.next_start,
            duration: dur,
            rendering_offset: 0,
            is_sync,
            bytes: bytes.into(),
        };
        let writer = self.writer.as_mut().ok_or("mp4 writer not open")?;
        writer
            .write_sample(1, &sample)
            .map_err(|e| format!("mp4 write: {e}"))?;
        self.next_start += u64::from(dur);
        Ok(())
    }
}

/// Split an Annex-B byte stream into NAL units (start codes stripped).
/// Handles 3- and 4-byte start codes (`00 00 01` / `00 00 00 01`) and
/// strips separator trailing zeros from each unit.
pub fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    // Positions of the 3-byte start codes; a 4-byte code's extra zero
    // shows up as a trailing zero of the previous unit and is trimmed.
    let mut codes = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            codes.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(codes.len());
    for (k, &sc) in codes.iter().enumerate() {
        let start = sc + 3;
        let raw_end = if k + 1 < codes.len() {
            codes[k + 1]
        } else {
            data.len()
        };
        let mut end = raw_end;
        while end > start && data[end - 1] == 0 {
            end -= 1;
        }
        if end > start {
            out.push(&data[start..end]);
        }
    }
    out
}

/// H.264 NAL unit type (`nal_ref` bits 0..5 of the header byte).
fn nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|b| b & 0x1F)
}

/// Repackage Annex-B NAL units as AVCC (4-byte length prefixes), dropping
/// SPS/PPS — they live in the MP4's `avcC` decoder-config record.
fn to_avcc(nals: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for nal in nals {
        let Some(t) = nal_type(nal) else { continue };
        if t == 7 || t == 8 {
            continue;
        }
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

/// File-stem timestamp: `YYYY-MM-DD-HHMMSS` in UTC.
fn timestamp_stem() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's
/// `civil_from_days` (public domain).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (mp + if mp < 10 { 3 } else { -9 }) as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Filesystem-safe peer name (≤24 chars of `[A-Za-z0-9._-]`).
fn sanitize(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    s.truncate(24);
    if s.is_empty() {
        s.push_str("viewer");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sc4() -> [u8; 4] {
        [0, 0, 0, 1]
    }

    fn sc3() -> [u8; 3] {
        [0, 0, 1]
    }

    #[test]
    fn splits_3_and_4_byte_start_codes() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&sc4());
        buf.extend_from_slice(&[0x65, 1, 2, 3]);
        buf.extend_from_slice(&sc3());
        buf.extend_from_slice(&[0x41, 9]);
        let nals = split_nals(&buf);
        assert_eq!(nals, vec![&[0x65u8, 1, 2, 3][..], &[0x41u8, 9][..]]);
    }

    #[test]
    fn trims_trailing_separator_zeros() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&sc4());
        buf.extend_from_slice(&[0x67, 5, 6, 0, 0]); // 4-byte code ahead → zeros are separators
        buf.extend_from_slice(&sc4());
        buf.extend_from_slice(&[0x68, 7]);
        buf.extend_from_slice(&[0, 0, 0]); // stream padding
        let nals = split_nals(&buf);
        assert_eq!(nals, vec![&[0x67u8, 5, 6][..], &[0x68u8, 7][..]]);
    }

    #[test]
    fn no_start_codes_yields_nothing() {
        assert!(split_nals(&[]).is_empty());
        assert!(split_nals(&[1, 2, 3, 0, 0]).is_empty());
        // A bare start code with no payload yields no NAL.
        assert!(split_nals(&sc4()).is_empty());
    }

    #[test]
    fn avcc_drops_parameter_sets_and_length_prefixes() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&sc4());
        buf.extend_from_slice(&[0x67, 0x64, 0x00, 0x1f]); // SPS (type 7)
        buf.extend_from_slice(&sc4());
        buf.extend_from_slice(&[0x68, 0xeb, 0xec]); // PPS (type 8)
        buf.extend_from_slice(&sc4());
        buf.extend_from_slice(&[0x65, 0x88, 0x84]); // IDR (type 5)
        let nals = split_nals(&buf);
        let avcc = to_avcc(&nals);
        // 4-byte BE length + payload, SPS/PPS skipped.
        assert_eq!(avcc, vec![0, 0, 0, 3, 0x65, 0x88, 0x84]);
    }

    #[test]
    fn records_then_reads_back_as_mp4() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = SessionRecorder::start(dir.path(), "work/pc|1", 30).unwrap();
        let t0 = Instant::now();

        // Frame 1: SPS + PPS + IDR (the recorder's trigger to open the file).
        let mut idr = Vec::new();
        idr.extend_from_slice(&sc4());
        idr.extend_from_slice(&[0x67, 0x64, 0x00, 0x1f, 0xad, 0xb8, 0x41]);
        idr.extend_from_slice(&sc4());
        idr.extend_from_slice(&[0x68, 0xeb, 0xec, 0xb2]);
        idr.extend_from_slice(&sc4());
        idr.extend_from_slice(&[0x65, 0x88, 0x84, 0x21, 0x40]);
        assert!(rec.write(t0, &idr, 320, 200));

        // Frames 2-3: delta slices (3-byte start codes this time).
        let mut delta = Vec::new();
        delta.extend_from_slice(&sc3());
        delta.extend_from_slice(&[0x41, 0x9a, 0x02, 0x05]);
        assert!(rec.write(t0 + std::time::Duration::from_millis(33), &delta, 320, 200));
        let mut delta2 = delta.clone();
        delta2.extend_from_slice(&[0xff]);
        assert!(rec.write(t0 + std::time::Duration::from_millis(66), &delta2, 320, 200));

        assert!(rec.finish().is_ok());
        let path = dir.path().join(rec.path().file_name().unwrap());
        assert!(path.exists(), "recording must exist: {}", path.display());

        let file = std::fs::File::open(&path).unwrap();
        let mut reader = mp4::read_mp4(file).expect("muxed file must parse as MP4");
        assert_eq!(
            reader.sample_count(1).unwrap(),
            3,
            "3 video samples expected"
        );
        let s1 = reader.read_sample(1, 1).unwrap().unwrap();
        assert!(s1.is_sync, "first sample must be the IDR");
        assert_eq!(s1.bytes.len(), 4 + 5, "IDR payload length-prefixed");
    }

    #[test]
    fn resolution_change_rotates_to_a_new_part_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = SessionRecorder::start(dir.path(), "viewer", 30).unwrap();
        let t0 = Instant::now();

        let mut key = Vec::new();
        key.extend_from_slice(&sc4());
        key.extend_from_slice(&[0x67, 0x64, 0x00, 0x1f, 0xad, 0xb8, 0x41]);
        key.extend_from_slice(&sc4());
        key.extend_from_slice(&[0x68, 0xeb, 0xec]);
        key.extend_from_slice(&sc4());
        key.extend_from_slice(&[0x65, 0x01, 0x02]);
        assert!(rec.write(t0, &key, 320, 200));

        // Same geometry, different stream size → rotate.
        let mut key2 = key.clone();
        key2.extend_from_slice(&[0x99]);
        assert!(rec.write(t0 + std::time::Duration::from_millis(40), &key2, 640, 400));
        assert!(rec.finish().is_ok());

        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "expected 2 part files, got {names:?}");
        assert!(names.iter().any(|n| n.ends_with("-part1.mp4")));
        for name in names {
            let file = std::fs::File::open(dir.path().join(name)).unwrap();
            let reader = mp4::read_mp4(file).unwrap();
            assert_eq!(reader.sample_count(1).unwrap(), 1);
        }
    }

    #[test]
    fn frames_before_first_keyframe_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = SessionRecorder::start(dir.path(), "viewer", 30).unwrap();
        let t0 = Instant::now();

        let mut delta = Vec::new();
        delta.extend_from_slice(&sc3());
        delta.extend_from_slice(&[0x41, 1, 2, 3]);
        assert!(rec.write(t0, &delta, 320, 200), "buffered, not an error");
        assert!(rec.write(t0, &delta, 320, 200));

        let mut idr = Vec::new();
        idr.extend_from_slice(&sc4());
        idr.extend_from_slice(&[0x67, 0x64, 0x00, 0x1f]);
        idr.extend_from_slice(&sc4());
        idr.extend_from_slice(&[0x68, 0xeb, 0xec]);
        idr.extend_from_slice(&sc4());
        idr.extend_from_slice(&[0x65, 0x10]);
        assert!(rec.write(t0, &idr, 320, 200));
        assert!(rec.finish().is_ok());

        let file = std::fs::File::open(dir.path().join(rec.path().file_name().unwrap())).unwrap();
        let mut reader = mp4::read_mp4(file).unwrap();
        assert_eq!(reader.sample_count(1).unwrap(), 1, "only the IDR is muxed");
        assert!(reader.read_sample(1, 1).unwrap().unwrap().is_sync);
    }

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        // 54 years = 54*365 + 13 leap days (1972..2020).
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        // +366 (2024) +365 (2025) +304 (Jan..Oct) → 2026-11-01.
        assert_eq!(civil_from_days(20_758), (2026, 11, 1));
    }

    #[test]
    fn peer_names_are_sanitized() {
        assert_eq!(sanitize("work-pc.office"), "work-pc.office");
        assert_eq!(sanitize("../etc/passwd"), ".._etc_passwd");
        assert_eq!(sanitize("итог"), "____");
        assert_eq!(sanitize(""), "viewer");
        let long = sanitize("a-very-long-computer-name-from-settings");
        assert!(long.len() <= 24);
    }
}
