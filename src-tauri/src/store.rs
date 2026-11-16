//! Local persistence: settings and the offline upload queue as JSON files in
//! the app data directory; the device token in the OS credential store (macOS
//! Keychain, Windows Credential Manager, Secret Service on Linux).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::activity::Sample;
use crate::api::Policy;

pub const DEFAULT_SERVER: &str = "https://monitor.sentroy.com";
const KEYCHAIN_SERVICE: &str = "com.sentroy.monitor";
/// Bound on the offline queue (~10 days of one-minute samples at 8h/day).
pub const MAX_QUEUE: usize = 5000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub server_url: String,
    pub consent_accepted: bool,
    pub user_name: Option<String>,
    pub company_name: Option<String>,
    pub company_slug: Option<String>,
    /// Local `YYYY-MM-DD` and the seconds tracked on it, for the "today" counter.
    pub today: Option<String>,
    pub today_seconds: u64,
    /// Seconds per app tracked on `today`, from finished samples.
    pub today_apps: Vec<TodayApp>,
    /// Activity per wall-clock ten-minute slot, most recent last.
    pub bars: Vec<Bar>,
    /// Last policy received from the server (used until the next refresh).
    pub policy: Policy,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TodayApp {
    pub name: String,
    pub seconds: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Bar {
    /// Unix seconds / 600.
    pub slot: i64,
    pub tracked: u32,
    pub active: u32,
    pub idle: u32,
}

/// Number of ten-minute bars the UI shows.
pub const BARS: usize = 6;

impl Settings {
    /// Starts a new day's counters when the local date changed.
    pub fn roll_day(&mut self, day: &str) {
        if self.today.as_deref() != Some(day) {
            self.today = Some(day.to_string());
            self.today_seconds = 0;
            self.today_apps.clear();
        }
    }

    /// Adds finished-sample seconds to an app's total for today.
    pub fn add_app_time(&mut self, name: &str, seconds: u64) {
        match self.today_apps.iter_mut().find(|a| a.name == name) {
            Some(a) => a.seconds += seconds,
            None => self.today_apps.push(TodayApp { name: name.to_string(), seconds }),
        }
    }

    /// Records one tracked second into the ten-minute bar of `slot`.
    pub fn add_bar_second(&mut self, slot: i64, active: bool, idle: bool) {
        if self.bars.last().map(|b| b.slot) != Some(slot) {
            self.bars.push(Bar { slot, ..Default::default() });
        }
        let b = self.bars.last_mut().expect("just pushed");
        b.tracked += 1;
        b.active += active as u32;
        b.idle += idle as u32;
        self.bars.retain(|b| b.slot > slot - BARS as i64);
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            server_url: DEFAULT_SERVER.into(),
            consent_accepted: false,
            user_name: None,
            company_name: None,
            company_slug: None,
            today: None,
            today_seconds: 0,
            today_apps: Vec::new(),
            bars: Vec::new(),
            policy: Policy::default(),
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
        let mut a = Accumulator::new(t);
        a.tick(0.0, None, None);
        a.finish(t + chrono::Duration::seconds(60)).unwrap()
    }

    #[test]
    fn settings_and_queue_round_trip() {
        let dir = std::env::temp_dir().join(format!("sm-test-{}", uuid::Uuid::new_v4()));
        let store = Store::new(dir.clone());
        assert_eq!(store.load_settings().server_url, DEFAULT_SERVER);
        let s = Settings { consent_accepted: true, today_apps: vec![TodayApp { name: "Figma".into(), seconds: 60 }], ..Default::default() };
        store.save_settings(&s);
        let back = store.load_settings();
        assert!(back.consent_accepted);
        assert_eq!(back.today_apps, s.today_apps);
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
    fn settings_from_before_projects_were_removed_still_load() {
        let old = r#"{"serverUrl":"https://x.test","consentAccepted":true,"projectId":"p","taskId":"t",
            "today":"2026-11-11","todaySeconds":90,"todayTasks":[{"taskId":"t","title":"Design","seconds":90}]}"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert!(s.consent_accepted);
        assert_eq!((s.server_url.as_str(), s.today_seconds), ("https://x.test", 90));
        assert!(s.today_apps.is_empty());
    }

    #[test]
    fn today_apps_accumulate_and_reset_on_a_new_day() {
        let mut s = Settings::default();
        s.roll_day("2026-11-11");
        s.add_app_time("Figma", 60);
        s.add_app_time("Safari", 30);
        s.add_app_time("Figma", 45);
        assert_eq!(s.today_apps, vec![TodayApp { name: "Figma".into(), seconds: 105 }, TodayApp { name: "Safari".into(), seconds: 30 }]);
        s.roll_day("2026-11-11");
        assert_eq!(s.today_apps.len(), 2);
        s.roll_day("2026-11-12");
        assert!(s.today_apps.is_empty());
    }

    #[test]
    fn bars_keep_the_last_six_slots() {
        let mut s = Settings::default();
        for slot in 0..10 {
            s.add_bar_second(slot, true, false);
            s.add_bar_second(slot, false, false);
        }
        assert_eq!(s.bars.len(), BARS);
        assert_eq!(s.bars[0].slot, 4);
        assert_eq!((s.bars[5].tracked, s.bars[5].active), (2, 1));
        // A gap drops old slots too.
        s.add_bar_second(100, false, true);
        assert_eq!(s.bars.len(), 1);
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
