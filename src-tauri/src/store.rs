//! Local persistence: settings and the offline upload queue as JSON files in
//! the app data directory; the device token in the macOS Keychain.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::activity::Sample;

pub const DEFAULT_SERVER: &str = "https://monitor.sentroy.com";
const KEYCHAIN_SERVICE: &str = "com.sentroy.monitor";
/// Bound on the offline queue (~10 days of one-minute samples at 8h/day).
pub const MAX_QUEUE: usize = 5000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub server_url: String,
    pub consent_accepted: bool,
    pub project_id: Option<String>,
    pub task_id: Option<String>,
    pub user_name: Option<String>,
    pub company_name: Option<String>,
    pub company_slug: Option<String>,
    /// Local `YYYY-MM-DD` and the seconds tracked on it, for the "today" counter.
    pub today: Option<String>,
    pub today_seconds: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            server_url: DEFAULT_SERVER.into(),
            consent_accepted: false,
            project_id: None,
            task_id: None,
            user_name: None,
            company_name: None,
            company_slug: None,
            today: None,
            today_seconds: 0,
        }
    }
}

pub struct Store {
    dir: PathBuf,
}

/// Write to a temp file, then rename, so a crash never leaves half a file.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&dir);
        Store { dir }
    }

    pub fn load_settings(&self) -> Settings {
        fs::read(self.dir.join("settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save_settings(&self, s: &Settings) {
        if let Ok(b) = serde_json::to_vec_pretty(s) {
            let _ = write_atomic(&self.dir.join("settings.json"), &b);
        }
    }

    pub fn load_queue(&self) -> Vec<Sample> {
        fs::read(self.dir.join("queue.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save_queue(&self, q: &[Sample]) {
        if let Ok(b) = serde_json::to_vec(q) {
            let _ = write_atomic(&self.dir.join("queue.json"), &b);
        }
    }

    fn entry(server: &str) -> Option<keyring::Entry> {
        keyring::Entry::new(KEYCHAIN_SERVICE, server).ok()
    }

    pub fn load_token(server: &str) -> Option<String> {
        Self::entry(server)?.get_password().ok()
    }

    pub fn save_token(server: &str, token: &str) -> bool {
        Self::entry(server).map(|e| e.set_password(token).is_ok()).unwrap_or(false)
    }

    pub fn delete_token(server: &str) {
        if let Some(e) = Self::entry(server) {
            let _ = e.delete_credential();
        }
    }
}

/// Appends to the queue, dropping the oldest samples beyond `MAX_QUEUE`.
pub fn push_bounded(queue: &mut Vec<Sample>, s: Sample) {
    queue.push(s);
    if queue.len() > MAX_QUEUE {
        let drop = queue.len() - MAX_QUEUE;
        queue.drain(..drop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::Accumulator;
    use chrono::{TimeZone, Utc};

    fn sample(i: i64) -> Sample {
        let t = Utc.with_ymd_and_hms(2026, 11, 11, 9, 0, 0).unwrap() + chrono::Duration::minutes(i);
        let mut a = Accumulator::new(t, None, None);
        a.tick(0.0, None);
        a.finish(t + chrono::Duration::seconds(60)).unwrap()
    }

    #[test]
    fn settings_and_queue_round_trip() {
        let dir = std::env::temp_dir().join(format!("sm-test-{}", uuid::Uuid::new_v4()));
        let store = Store::new(dir.clone());
        assert_eq!(store.load_settings().server_url, DEFAULT_SERVER);
        let s = Settings { consent_accepted: true, project_id: Some("p".into()), ..Default::default() };
        store.save_settings(&s);
        let back = store.load_settings();
        assert!(back.consent_accepted);
        assert_eq!(back.project_id.as_deref(), Some("p"));
        let q = vec![sample(0), sample(1)];
        store.save_queue(&q);
        assert_eq!(store.load_queue(), q);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_or_partial_settings_fall_back_to_defaults() {
        let s: Settings = serde_json::from_str(r#"{"consentAccepted":true,"future":1}"#).unwrap();
        assert!(s.consent_accepted);
        assert_eq!(s.server_url, DEFAULT_SERVER);
    }

    #[test]
    fn queue_is_bounded() {
        let mut q = Vec::new();
        for i in 0..(MAX_QUEUE as i64 + 3) {
            push_bounded(&mut q, sample(i));
        }
        assert_eq!(q.len(), MAX_QUEUE);
        assert_eq!(q[0].start, sample(3).start);
    }
}
