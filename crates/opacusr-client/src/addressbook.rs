//! Viewer-side address book and connection history.
//!
//! Stored as a small TOML file next to the agent config
//! (`<config_dir>/opacusr/addressbook.toml`) — no server, no accounts.
//! Passwords are NEVER stored here: they are per-host secrets the user
//! types on connect.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Monotonic-ish unique id for contacts (unix seconds + process counter).
static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Maximum number of remembered history entries.
pub const HISTORY_CAP: usize = 100;

/// One saved remote (a "contact").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Contact {
    pub id: String,
    pub label: String,
    /// 9-digit host ID or `ip:port` (LAN mode).
    pub target: String,
    /// Optional signaling endpoint tried BEFORE the global list —
    /// e.g. this host's private tunnel (`wss://…/signal`).
    pub endpoint: Option<String>,
    pub favorite: bool,
    /// Unix seconds of the last successful connection.
    pub last_used: Option<u64>,
}

impl Default for Contact {
    fn default() -> Self {
        Self {
            id: new_id(),
            label: String::new(),
            target: String::new(),
            endpoint: None,
            favorite: false,
            last_used: None,
        }
    }
}

/// How a connection attempt ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Handshake completed; media was flowing.
    Connected,
    /// Never reached the host.
    Failed,
    /// Attempt still in progress (or the app closed mid-attempt).
    #[default]
    Unknown,
}

/// One row of the connection history (newest first in storage).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryEntry {
    /// Unix seconds — when the attempt started.
    pub ts: u64,
    /// Contact label, if the attempt came from the address book.
    pub label: Option<String>,
    /// Target as typed (ID or ip:port).
    pub target: String,
    pub outcome: Outcome,
    /// Session length in seconds, when it actually connected.
    pub duration_secs: Option<u64>,
}

/// Contacts + history, persisted as TOML.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AddressBook {
    contacts: Vec<Contact>,
    history: Vec<HistoryEntry>,
}

impl AddressBook {
    /// Default storage path: `<config_dir>/opacusr/addressbook.toml`.
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("opacusr")
            .join("addressbook.toml")
    }

    /// Load from the default path; missing file → empty book.
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    /// Load from a path. A missing OR corrupt file yields an empty book:
    /// a broken address book must never block connecting.
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(book) => book,
                Err(e) => {
                    tracing::warn!(
                        "addressbook {}: corrupt, starting empty ({e})",
                        path.display()
                    );
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// Atomic save: write a sibling `.tmp` file, then rename over the target.
    pub fn save(&self) -> io::Result<()> {
        self.save_to(&Self::path())
    }

    /// Atomic save to an explicit path (tests, custom locations).
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        // Same-directory rename — atomic on all supported platforms.
        std::fs::rename(&tmp, path)
    }

    // ------------------------------------------------------------------
    // Contacts
    // ------------------------------------------------------------------

    pub fn contacts(&self) -> &[Contact] {
        &self.contacts
    }

    /// Insert or update by normalized target. The label is replaced,
    /// the endpoint is replaced (`None` clears it). Returns the id.
    pub fn upsert(&mut self, label: String, target: String, endpoint: Option<String>) -> String {
        let norm = normalize_target(&target);
        if let Some(c) = self
            .contacts
            .iter_mut()
            .find(|c| normalize_target(&c.target) == norm)
        {
            c.label = label;
            c.endpoint = endpoint;
            return c.id.clone();
        }
        let contact = Contact {
            label,
            target,
            endpoint,
            ..Default::default()
        };
        let id = contact.id.clone();
        self.contacts.push(contact);
        id
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.contacts.len();
        self.contacts.retain(|c| c.id != id);
        self.contacts.len() != before
    }

    pub fn set_favorite(&mut self, id: &str, favorite: bool) -> bool {
        match self.contacts.iter_mut().find(|c| c.id == id) {
            Some(c) => {
                c.favorite = favorite;
                true
            }
            None => false,
        }
    }

    /// Mark a contact as used right now (matched by target).
    pub fn touch_by_target(&mut self, target: &str) -> bool {
        let norm = normalize_target(target);
        match self
            .contacts
            .iter_mut()
            .find(|c| normalize_target(&c.target) == norm)
        {
            Some(c) => {
                c.last_used = Some(now_secs());
                true
            }
            None => false,
        }
    }

    /// Favorites first, then most recently used, then alphabetical.
    pub fn sorted_contacts(&self) -> Vec<Contact> {
        let mut v = self.contacts.clone();
        v.sort_by(|a, b| {
            b.favorite
                .cmp(&a.favorite)
                .then(b.last_used.cmp(&a.last_used))
                .then(a.label.to_lowercase().cmp(&b.label.to_lowercase()))
                .then(a.id.cmp(&b.id))
        });
        v
    }

    // ------------------------------------------------------------------
    // History
    // ------------------------------------------------------------------

    pub fn history(&self) -> &[HistoryEntry] {
        &self.history
    }

    /// Prepend (newest first) and cap at [`HISTORY_CAP`].
    pub fn push_history(&mut self, entry: HistoryEntry) {
        self.history.insert(0, entry);
        self.history.truncate(HISTORY_CAP);
    }

    /// Set the outcome of an in-progress attempt (found by ts).
    pub fn mark_outcome(&mut self, ts: u64, outcome: Outcome) -> bool {
        match self.history.iter_mut().find(|h| h.ts == ts) {
            Some(h) => {
                h.outcome = outcome;
                true
            }
            None => false,
        }
    }

    /// Finalize an attempt: outcome + session duration (if not already set).
    pub fn finish_history(
        &mut self,
        ts: u64,
        outcome: Outcome,
        duration_secs: Option<u64>,
    ) -> bool {
        match self.history.iter_mut().find(|h| h.ts == ts) {
            Some(h) => {
                h.outcome = outcome;
                if h.duration_secs.is_none() {
                    h.duration_secs = duration_secs;
                }
                true
            }
            None => false,
        }
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
    }
}

/// Normalize a target for dedupe: strip spaces/dashes, lowercase.
fn normalize_target(t: &str) -> String {
    t.chars()
        .filter(|c| !matches!(c, ' ' | '-'))
        .collect::<String>()
        .to_lowercase()
}

/// Current unix time in seconds (0 if the clock is before the epoch).
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn new_id() -> String {
    let n = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:x}-{:x}", now_secs(), n)
}

/// `1_700_000_000` → `"2023-11-14 22:13"` (UTC). Pure integer math —
/// no chrono dependency (Howard Hinnant's civil_from_days).
pub fn fmt_ts(ts: u64) -> String {
    let days = (ts / 86_400) as i64;
    let secs_of_day = ts % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60
    )
}

/// `204` → `"3m 24s"`, `59` → `"59s"`, `7292` → `"2h 01m"`.
pub fn fmt_duration(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "opacusr-ab-test-{name}-{}.toml",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn roundtrip_save_load() {
        let path = tmp_path("roundtrip");
        let mut book = AddressBook::default();
        let id = book.upsert(
            "Home PC".into(),
            "123-456 789".into(),
            Some("wss://t/signal".into()),
        );
        book.set_favorite(&id, true);
        book.push_history(HistoryEntry {
            ts: 1_700_000_000,
            label: Some("Home PC".into()),
            target: "123456789".into(),
            outcome: Outcome::Connected,
            duration_secs: Some(204),
        });
        book.save_to(&path).unwrap();
        let loaded = AddressBook::load_from(&path);
        assert_eq!(book, loaded);
        assert_eq!(loaded.contacts()[0].target, "123-456 789");
        // The .tmp sibling must not linger after a successful save.
        assert!(!path.with_extension("toml.tmp").exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn upsert_dedupes_by_normalized_target() {
        let mut book = AddressBook::default();
        book.upsert("A".into(), "123-456 789".into(), None);
        let id2 = book.upsert("B".into(), "123456789".into(), Some("wss://x".into()));
        assert_eq!(book.contacts().len(), 1);
        let c = &book.contacts()[0];
        assert_eq!(c.id, id2);
        assert_eq!(c.label, "B");
        assert_eq!(c.endpoint.as_deref(), Some("wss://x"));
    }

    #[test]
    fn sorted_favorites_first_then_recent() {
        let mut book = AddressBook::default();
        let a = book.upsert("A".into(), "111111111".into(), None);
        let b = book.upsert("B".into(), "222222222".into(), None);
        let c = book.upsert("C".into(), "333333333".into(), None);
        book.set_favorite(&c, true);
        book.touch_by_target("222222222"); // b is the recent one
        let order: Vec<String> = book.sorted_contacts().into_iter().map(|x| x.id).collect();
        assert_eq!(order, vec![c, b, a]);
    }

    #[test]
    fn history_is_capped_and_newest_first() {
        let mut book = AddressBook::default();
        for i in 0u64..(HISTORY_CAP as u64 + 50) {
            book.push_history(HistoryEntry {
                ts: i,
                label: None,
                target: "x".into(),
                outcome: Outcome::Unknown,
                duration_secs: None,
            });
        }
        assert_eq!(book.history().len(), HISTORY_CAP);
        assert_eq!(book.history()[0].ts, HISTORY_CAP as u64 + 49);
        assert_eq!(book.history().last().unwrap().ts, 50);
    }

    #[test]
    fn mark_and_finish_history() {
        let mut book = AddressBook::default();
        book.push_history(HistoryEntry {
            ts: 42,
            label: None,
            target: "t".into(),
            outcome: Outcome::Unknown,
            duration_secs: None,
        });
        assert!(book.mark_outcome(42, Outcome::Connected));
        assert!(book.finish_history(42, Outcome::Connected, Some(65)));
        assert_eq!(book.history()[0].outcome, Outcome::Connected);
        assert_eq!(book.history()[0].duration_secs, Some(65));
        // Unknown ts is a no-op.
        assert!(!book.mark_outcome(999, Outcome::Failed));
        assert!(!book.finish_history(999, Outcome::Failed, None));
    }

    #[test]
    fn missing_and_corrupt_files_load_empty() {
        let path = tmp_path("missing");
        assert!(AddressBook::load_from(&path).contacts().is_empty());
        std::fs::write(&path, "not [valid toml ===").unwrap();
        assert!(AddressBook::load_from(&path).contacts().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn remove_and_favorite() {
        let mut book = AddressBook::default();
        let id = book.upsert("A".into(), "111111111".into(), None);
        assert!(book.set_favorite(&id, true));
        assert!(book.contacts()[0].favorite);
        assert!(!book.set_favorite("nope", true));
        assert!(book.remove(&id));
        assert!(!book.remove(&id));
        assert!(book.contacts().is_empty());
    }

    #[test]
    fn fmt_helpers() {
        assert_eq!(fmt_duration(59), "59s");
        assert_eq!(fmt_duration(204), "3m 24s");
        assert_eq!(fmt_duration(7_292), "2h 01m");
        assert_eq!(fmt_ts(0), "1970-01-01 00:00");
        assert_eq!(fmt_ts(1_700_000_000), "2023-11-14 22:13");
    }
}
