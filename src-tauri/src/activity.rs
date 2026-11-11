//! Activity sampling. Privacy by construction: we only ask the OS (see `os`)
//! how long ago the last keyboard/mouse event happened and which app is in
//! front. No key codes, window titles or URLs are ever read, so no
//! Accessibility / Input Monitoring permission is needed.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A second counts as "active" when the last input happened within this window.
pub const ACTIVE_WINDOW_SECS: f64 = 1.5;
/// No input for this long and the time counts as idle.
pub const IDLE_THRESHOLD_SECS: f64 = 300.0;
/// Samples are cut at this length (the server accepts up to 15 minutes).
pub const SAMPLE_SECS: i64 = 60;
/// Used when the OS cannot report input idle time (pure Wayland): the second is
/// tracked but counts as neither active nor idle, so it never triggers the
/// idle auto-stop.
pub const UNKNOWN_INPUT_SECS: f64 = 60.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppUsage {
    pub name: String,
    pub seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    pub id: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub project_id: Option<String>,
    pub task_id: Option<String>,
    pub active_seconds: u32,
    pub idle_seconds: u32,
    pub apps: Vec<AppUsage>,
}

/// Accumulates one-second ticks into a sample.
#[derive(Debug)]
pub struct Accumulator {
    start: DateTime<Utc>,
    project_id: Option<String>,
    task_id: Option<String>,
    ticks: u32,
    active: u32,
    idle: u32,
    apps: HashMap<String, u32>,
}

impl Accumulator {
    pub fn new(start: DateTime<Utc>, project_id: Option<String>, task_id: Option<String>) -> Self {
        Accumulator { start, project_id, task_id, ticks: 0, active: 0, idle: 0, apps: HashMap::new() }
    }

    /// One second of tracking. `since_input` is seconds since the last input event.
    pub fn tick(&mut self, since_input: f64, front_app: Option<&str>) {
        self.ticks += 1;
        if since_input >= IDLE_THRESHOLD_SECS {
            self.idle += 1;
            return; // idle time is not attributed to an app
        }
        if since_input < ACTIVE_WINDOW_SECS {
            self.active += 1;
        }
        if let Some(name) = front_app {
            let name: String = name.chars().filter(|c| !c.is_control()).take(120).collect();
            if !name.is_empty() {
                *self.apps.entry(name).or_insert(0) += 1;
            }
        }
    }

    pub fn tracked_seconds(&self) -> u32 {
        self.ticks
    }

    pub fn task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }

    /// Closes the sample at `end`. Counts are clamped to the wall-clock span so a
    /// late tick (sleep/wake) can never produce more seconds than elapsed.
    pub fn finish(self, end: DateTime<Utc>) -> Option<Sample> {
        if self.ticks == 0 {
            return None;
        }
        let span = (end - self.start).num_seconds().max(1) as u32;
        let span = span.min(15 * 60);
        let end = self.start + chrono::Duration::seconds(span as i64);
        let idle = self.idle.min(span);
        let active = self.active.min(span - idle);
        let mut apps: Vec<AppUsage> =
            self.apps.into_iter().map(|(name, seconds)| AppUsage { name, seconds: seconds.min(span) }).collect();
        apps.sort_by(|a, b| b.seconds.cmp(&a.seconds).then_with(|| a.name.cmp(&b.name)));
        apps.truncate(20);
        Some(Sample {
            id: uuid::Uuid::new_v4().simple().to_string(),
            start: self.start,
            end,
            project_id: self.project_id,
            task_id: self.task_id,
            active_seconds: active,
            idle_seconds: idle,
            apps,
        })
    }
}

/// Activity percent of non-idle time, as the web timeline shows it.
pub fn activity_percent(active: u32, tracked: u32, idle: u32) -> u32 {
    let working = tracked.saturating_sub(idle);
    if working == 0 {
        0
    } else {
        ((active as f64 / working as f64) * 100.0).round().min(100.0) as u32
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 11, 9, 0, 0).unwrap()
    }

    #[test]
    fn counts_active_idle_and_apps() {
        let mut a = Accumulator::new(t0(), Some("p".into()), None);
        for _ in 0..30 {
            a.tick(0.2, Some("Cursor"));
        }
        for _ in 0..20 {
            a.tick(10.0, Some("Safari")); // passive reading: not active, still Safari time
        }
        for _ in 0..10 {
            a.tick(400.0, Some("Safari")); // idle
        }
        let s = a.finish(t0() + chrono::Duration::seconds(60)).unwrap();
        assert_eq!((s.active_seconds, s.idle_seconds), (30, 10));
        assert_eq!(s.apps, vec![AppUsage { name: "Cursor".into(), seconds: 30 }, AppUsage { name: "Safari".into(), seconds: 20 }]);
        assert_eq!(s.project_id.as_deref(), Some("p"));
        assert_eq!(s.id.len(), 32);
    }

    #[test]
    fn clamps_to_wall_clock_after_sleep() {
        let mut a = Accumulator::new(t0(), None, None);
        for _ in 0..100 {
            a.tick(0.0, Some("X"));
        }
        let s = a.finish(t0() + chrono::Duration::seconds(40)).unwrap();
        assert_eq!(s.active_seconds, 40);
        assert_eq!(s.apps[0].seconds, 40);
        assert_eq!((s.end - s.start).num_seconds(), 40);
        // A huge gap is capped to the server's 15-minute limit.
        let mut b = Accumulator::new(t0(), None, None);
        b.tick(0.0, None);
        let s = b.finish(t0() + chrono::Duration::hours(3)).unwrap();
        assert_eq!((s.end - s.start).num_seconds(), 900);
    }

    #[test]
    fn empty_accumulator_yields_nothing_and_strips_control_chars() {
        assert!(Accumulator::new(t0(), None, None).finish(t0()).is_none());
        let mut a = Accumulator::new(t0(), None, None);
        a.tick(0.0, Some("Ev\u{7}il\n"));
        assert_eq!(a.finish(t0() + chrono::Duration::seconds(1)).unwrap().apps[0].name, "Evil");
    }

    #[test]
    fn serialises_like_the_server_expects() {
        let mut a = Accumulator::new(t0(), None, Some("t".into()));
        a.tick(0.0, Some("A"));
        let v = serde_json::to_value(a.finish(t0() + chrono::Duration::seconds(60)).unwrap()).unwrap();
        assert_eq!(v["start"], "2026-11-11T09:00:00Z");
        assert_eq!(v["taskId"], "t");
        assert_eq!(v["activeSeconds"], 1);
        assert!(v.get("projectId").is_some());
    }

    #[test]
    fn percent() {
        assert_eq!(activity_percent(30, 60, 0), 50);
        assert_eq!(activity_percent(30, 60, 30), 100);
        assert_eq!(activity_percent(0, 0, 0), 0);
    }
}
