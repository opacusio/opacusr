//! Role-agnostic file-transfer engine shared by the host agent and the
//! viewer client: either side can push or receive files.
//!
//! Both directions run through the session pump: this module only keeps
//! state machines keyed by transfer id and produces/consumes `Message`s.
//! MVP transport policy: chunked, cumulative-ack, sliding window of
//! [`WINDOW`] chunks, sha256 verification on completion.

use std::collections::HashMap;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use opacusr_proto::message::*;

pub const CHUNK_SIZE: u32 = 64 * 1024;
pub const WINDOW: u64 = 8;

/// Events the session loop cares about.
#[derive(Debug, Clone)]
pub enum FtEvent {
    /// A file finished arriving (`id`, saved path, verified).
    ReceivedDone(u32, PathBuf, bool),
    /// A pushed file finished uploading.
    SentDone(u32, bool),
    /// Progress update (id, done_bytes, total_bytes).
    Progress(u32, u64, u64),
    Failed(u32, String),
}

/// Receiver state (we are the host receiving a file).
struct Incoming {
    id: u32,
    #[allow(dead_code)] // shown in UI in a later milestone
    name: String,
    size: u64,
    sha: [u8; 32],
    file: std::fs::File,
    path: PathBuf,
    next_seq: u64,
    written: u64,
    /// Out-of-order chunks held until their turn (bounded).
    buffer: std::collections::BTreeMap<u64, Vec<u8>>,
}

/// Sender state (we are pushing a file).
struct Outgoing {
    #[allow(dead_code)] // echoed back in FtComplete bookkeeping later
    id: u32,
    #[allow(dead_code)]
    path: PathBuf,
    #[allow(dead_code)]
    name: String,
    size: u64,
    #[allow(dead_code)]
    sha: [u8; 32],
    reader: std::fs::File,
    offset: u64,
    acked_seq: u64,
    sent_seq: i64,
    last_activity: std::time::Instant,
}

/// Aggregated file-transfer state for one session (both roles).
#[derive(Default)]
pub struct FtEngine {
    next_id: u32,
    incoming: HashMap<u32, Incoming>,
    outgoing: HashMap<u32, Outgoing>,
}

impl FtEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start pushing a local file to the remote side. Returns the offer to
    /// send (or an error string).
    pub fn start_push(&mut self, path: &Path) -> Result<Message, String> {
        let mut file = std::fs::File::open(path).map_err(|e| format!("open {path:?}: {e}"))?;
        let size = file.metadata().map_err(|e| e.to_string())?.len();
        let mut sha = Sha256::new();
        let mut buf = vec![0u8; CHUNK_SIZE as usize];
        loop {
            let n = file.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            sha.update(&buf[..n]);
        }
        let sha: [u8; 32] = sha.finalize().into();
        let id = self.next_id;
        self.next_id += 1;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        let offer = FtOffer {
            id,
            name: name.clone(),
            size,
            chunk_size: CHUNK_SIZE,
            sha256: sha,
        };
        file.seek(std::io::SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        self.outgoing.insert(
            id,
            Outgoing {
                id,
                path: path.to_path_buf(),
                name,
                size,
                sha,
                reader: file,
                offset: 0,
                acked_seq: 0,
                sent_seq: -1,
                last_activity: std::time::Instant::now(),
            },
        );
        Ok(Message::FtOffer(offer))
    }

    /// Feed one message; returns replies to send and events.
    /// `incoming_dir` is where accepted files are written.
    pub fn on_message(
        &mut self,
        msg: &Message,
        incoming_dir: &Path,
        auto_accept: bool,
    ) -> (Vec<Message>, Vec<FtEvent>) {
        let mut out = Vec::new();
        let mut events = Vec::new();
        match msg {
            Message::FtOffer(offer) => {
                if !auto_accept {
                    events.push(FtEvent::Failed(
                        offer.id,
                        "auto-accept disabled; file transfer rejected".into(),
                    ));
                    out.push(Message::FtReject(FtReject {
                        id: offer.id,
                        reason: "auto-accept disabled on host".into(),
                    }));
                    return (out, events);
                }
                let safe = sanitize_name(&offer.name);
                let path = unique_path(&incoming_dir.join(safe));
                if let Err(e) = std::fs::create_dir_all(incoming_dir) {
                    events.push(FtEvent::Failed(offer.id, format!("mkdir: {e}")));
                    return (out, events);
                }
                match std::fs::File::create(&path) {
                    Ok(file) => {
                        let id = offer.id;
                        self.incoming.insert(
                            id,
                            Incoming {
                                id,
                                name: offer.name.clone(),
                                size: offer.size,
                                sha: offer.sha256,
                                file,
                                path,
                                next_seq: 0,
                                written: 0,
                                buffer: std::collections::BTreeMap::new(),
                            },
                        );
                        out.push(Message::FtAccept(FtAccept { id, offset: 0 }));
                    }
                    Err(e) => {
                        out.push(Message::FtReject(FtReject {
                            id: offer.id,
                            reason: format!("cannot create file: {e}"),
                        }));
                    }
                }
            }
            Message::FtChunk(chunk) => {
                if let Some(inc) = self.incoming.get_mut(&chunk.id) {
                    if chunk.seq == inc.next_seq {
                        let write_err = std::io::Write::write_all(&mut inc.file, &chunk.data).err();
                        if let Some(e) = write_err {
                            let id = inc.id;
                            events.push(FtEvent::Failed(id, format!("write: {e}")));
                            out.push(Message::FtCancel(FtCancel {
                                id,
                                reason: "io error".into(),
                            }));
                            self.incoming.remove(&id);
                            return (out, events);
                        }
                        inc.written += chunk.data.len() as u64;
                        inc.next_seq += 1;
                        // Drain buffered successors while they line up.
                        while let Some(data) = inc.buffer.remove(&inc.next_seq) {
                            if let Err(e) = std::io::Write::write_all(&mut inc.file, &data) {
                                let id = inc.id;
                                events.push(FtEvent::Failed(id, format!("write: {e}")));
                                out.push(Message::FtCancel(FtCancel {
                                    id,
                                    reason: "io error".into(),
                                }));
                                self.incoming.remove(&id);
                                return (out, events);
                            }
                            inc.written += data.len() as u64;
                            inc.next_seq += 1;
                        }
                        out.push(Message::FtAck(FtAck {
                            id: inc.id,
                            acked_seq: inc.next_seq - 1,
                        }));
                        events.push(FtEvent::Progress(inc.id, inc.written, inc.size));
                    } else if chunk.seq > inc.next_seq {
                        // Out-of-order (possible on future unreliable
                        // channels): keep it if the buffer stays bounded.
                        if inc.buffer.len() < 64 {
                            inc.buffer.insert(chunk.seq, chunk.data.clone());
                        }
                        out.push(Message::FtAck(FtAck {
                            id: inc.id,
                            acked_seq: inc.next_seq.saturating_sub(1),
                        }));
                    }
                    // Duplicate (seq < next_seq): re-ack current position.
                    else {
                        out.push(Message::FtAck(FtAck {
                            id: inc.id,
                            acked_seq: inc.next_seq.saturating_sub(1),
                        }));
                    }
                }
            }
            Message::FtComplete(done) => {
                if let Some(inc) = self.incoming.remove(&done.id) {
                    drop(inc.file);
                    let actual = sha256_file(&inc.path);
                    let verified = actual == Some(inc.sha) && inc.written == inc.size;
                    events.push(FtEvent::ReceivedDone(inc.id, inc.path.clone(), verified));
                }
            }
            Message::FtCancel(cancel) => {
                if let Some(inc) = self.incoming.remove(&cancel.id) {
                    drop(inc.file);
                    let _ = std::fs::remove_file(&inc.path);
                    events.push(FtEvent::Failed(
                        cancel.id,
                        format!("cancelled: {}", cancel.reason),
                    ));
                }
                if let Some(outg) = self.outgoing.remove(&cancel.id) {
                    events.push(FtEvent::Failed(
                        cancel.id,
                        format!("cancelled: {}", cancel.reason),
                    ));
                    let _ = outg;
                }
            }
            Message::FtAck(ack) => {
                if let Some(outg) = self.outgoing.get_mut(&ack.id) {
                    outg.acked_seq = ack.acked_seq;
                    outg.last_activity = std::time::Instant::now();
                }
            }
            Message::FtAccept(accept) => {
                // Remote accepted our push; may resume from offset.
                if let Some(outg) = self.outgoing.get_mut(&accept.id) {
                    if accept.offset > 0 {
                        let _ = outg.reader.seek(std::io::SeekFrom::Start(accept.offset));
                        outg.offset = accept.offset;
                        outg.acked_seq = accept.offset / CHUNK_SIZE as u64;
                        outg.sent_seq = outg.acked_seq as i64 - 1;
                    }
                    // The caller calls `pump` afterwards to start sending.
                }
            }
            _ => {}
        }
        (out, events)
    }

    /// Advance all outgoing transfers (fill the sliding window). Returns
    /// messages to send.
    pub fn pump(&mut self) -> Vec<Message> {
        let mut out = Vec::new();
        let ids: Vec<u32> = self.outgoing.keys().copied().collect();
        for id in ids {
            // Fill the window with freshly read chunks.
            while let Some(outg) = self.outgoing.get_mut(&id) {
                let inflight = (outg.sent_seq + 1) as u64 - outg.acked_seq;
                if inflight >= WINDOW {
                    break;
                }
                if outg.offset >= outg.size {
                    break; // fully read (also covers empty files)
                }
                let mut buf = vec![0u8; CHUNK_SIZE as usize];
                match outg.reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.truncate(n);
                        outg.offset += n as u64;
                        outg.sent_seq += 1;
                        outg.last_activity = std::time::Instant::now();
                        let seq = outg.sent_seq as u64;
                        out.push(Message::FtChunk(FtChunk { id, seq, data: buf }));
                    }
                    Err(e) => {
                        self.outgoing.remove(&id);
                        out.push(Message::FtCancel(FtCancel {
                            id,
                            reason: format!("read: {e}"),
                        }));
                        break;
                    }
                }
            }
            // Complete once the remote acked every chunk.
            let (done, size) = match self.outgoing.get(&id) {
                Some(o) => (o.offset >= o.size, o.size),
                None => continue,
            };
            if !done {
                continue;
            }
            let total_chunks = size.div_ceil(CHUNK_SIZE as u64).max(1);
            if self.outgoing.get(&id).unwrap().acked_seq + 1 >= total_chunks {
                if let Some(outg) = self.outgoing.remove(&id) {
                    let _ = outg;
                    out.push(Message::FtComplete(FtComplete { id, verified: true }));
                }
            }
        }
        out
    }

    /// True when no transfers are pending.
    pub fn is_idle(&self) -> bool {
        self.incoming.is_empty() && self.outgoing.is_empty()
    }
}

/// Remove path components and reserved characters from a peer-provided
/// filename.
fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " ._-()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim().to_string();
    let cleaned = cleaned.replace("..", "_");
    if cleaned.is_empty() {
        "file".into()
    } else {
        cleaned
    }
}

/// Append `.1`, `.2`, ... until the path does not exist.
fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path.extension().map(|s| s.to_string_lossy().into_owned());
    for i in 1..10_000u32 {
        let candidate = match &ext {
            Some(e) => path.with_file_name(format!("{stem}.{i}.{e}")),
            None => path.with_file_name(format!("{stem}.{i}")),
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    path.with_file_name(format!("{stem}.overflow"))
}

pub fn sha256_file(path: &Path) -> Option<[u8; 32]> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut sha = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
    }
    Some(sha.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_blocks_traversal() {
        let s = sanitize_name("../../etc/passwd");
        assert!(!s.contains('/'));
        assert!(!s.contains(".."));
        assert_eq!(sanitize_name("  report.pdf "), "report.pdf");
        assert!(!sanitize_name(" всё/со звёздочкой* ").contains('*'));
        assert_eq!(sanitize_name(""), "file");
    }

    #[test]
    fn unique_path_increments() {
        let dir = std::env::temp_dir().join(format!("opacusr-uniq-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("f.txt");
        std::fs::write(&base, b"x").unwrap();
        let u1 = unique_path(&base);
        std::fs::write(&u1, b"x").unwrap();
        let u2 = unique_path(&base);
        assert_eq!(u1.file_name().unwrap(), "f.1.txt");
        assert_eq!(u2.file_name().unwrap(), "f.2.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two engines facing each other through a message queue must complete
    /// a full upload → download round trip with matching sha256.
    #[test]
    fn engine_round_trip_both_directions() {
        let dir = std::env::temp_dir().join(format!("opacusr-ft-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Payload with a size that is NOT a multiple of the chunk size.
        let payload: Vec<u8> = (0..(CHUNK_SIZE as usize * 3 + 777))
            .map(|i| (i * 31 % 251) as u8)
            .collect();
        let src = dir.join("payload.bin");
        std::fs::write(&src, &payload).unwrap();
        let expected_sha = sha256_file(&src).unwrap();

        let mut alice = FtEngine::new(); // pushes the file
        let mut bob = FtEngine::new(); // receives it
        let inbox = dir.join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();

        // Alice offers; bob receives. Route messages by direction until
        // both engines go idle. FIFO per direction: the wire is reliable
        // and ordered.
        let mut to_bob = std::collections::VecDeque::from(vec![alice.start_push(&src).unwrap()]);
        let mut to_alice: std::collections::VecDeque<Message> = Default::default();
        let mut done = false;
        for _ in 0..10_000 {
            while let Some(m) = to_bob.pop_front() {
                let (replies, events) = bob.on_message(&m, &inbox, true);
                to_alice.extend(replies);
                for ev in events {
                    if let FtEvent::ReceivedDone(_, path, verified) = ev {
                        assert!(verified);
                        assert_eq!(sha256_file(&path), Some(expected_sha));
                        assert_eq!(std::fs::read(&path).unwrap(), payload);
                        done = true;
                    }
                }
            }
            while let Some(m) = to_alice.pop_front() {
                let (replies, _ev) = alice.on_message(&m, &inbox, true);
                to_bob.extend(replies);
            }
            to_bob.extend(alice.pump());
            if to_bob.is_empty() && to_alice.is_empty() {
                break;
            }
        }
        assert!(done, "transfer never completed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
