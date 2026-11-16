//! Clock-in / clock-out events (`POST /api/agent/sessions`). A `start` is
//! queued when the timer starts and a `stop` (with why it stopped and, at the
//! end of the day, an optional note) when it stops. Like activity samples they
//! wait in a bounded on-disk queue until the uploader delivers them; the
//! client-made id makes retries idempotent on the server.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Bound on the on-disk queue (a normal day adds a handful of events).
pub const MAX_EVENTS: usize = 1000;
/// The server accepts at most this many events per request.
pub const UPLOAD_BATCH: usize = 500;
/// Longest note the server accepts, in characters.
pub const MAX_NOTE_CHARS: usize = 2000;
/// Events older than this are dropped instead of being kept for a server
/// that does not have the endpoint (yet).
pub const MAX_AGE_DAYS: i64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Start,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The user ended the day in the stop dialog.
    EndOfDay,
    /// "Be right back" in the stop dialog.
    Break,
    /// The idle auto-stop.
    Idle,
    /// The app quit, restarted or signed out under a running timer.
    Quit,
}

impl StopReason {
    /// Parses the reason the UI sends (`end_of_day`, `break`, …).
    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEvent {
    pub id: String,
    pub kind: Kind,
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<StopReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

impl SessionEvent {
    pub fn start(at: DateTime<Utc>) -> Self {
        SessionEvent { id: new_id(), kind: Kind::Start, at, reason: None, note: None }
    }

    /// A stop event. The note is only kept for `EndOfDay`, trimmed, never
    /// empty and cut to `MAX_NOTE_CHARS` characters.
    pub fn stop(at: DateTime<Utc>, reason: StopReason, note: Option<&str>) -> Self {
        let note = match reason {
            StopReason::EndOfDay => note
                .map(|n| n.trim().chars().take(MAX_NOTE_CHARS).collect::<String>())
                .map(|n| n.trim_end().to_string())
                .filter(|n| !n.is_empty()),
            _ => None,
        };
        SessionEvent { id: new_id(), kind: Kind::Stop, at, reason: Some(reason), note }
    }
}

/// Appends to the queue, dropping the oldest events beyond `MAX_EVENTS`.
pub fn push_bounded(queue: &mut Vec<SessionEvent>, e: SessionEvent) {
    queue.push(e);
    if queue.len() > MAX_EVENTS {
        let drop = queue.len() - MAX_EVENTS;
        queue.drain(..drop);
    }
}

/// Drops events older than `MAX_AGE_DAYS`. Returns whether anything was dropped.
pub fn prune_old(queue: &mut Vec<SessionEvent>, now: DateTime<Utc>) -> bool {
    let cutoff = now - Duration::days(MAX_AGE_DAYS);
    let before = queue.len();
    queue.retain(|e| e.at >= cutoff);
    queue.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    #[test]
    fn start_serializes_without_optional_fields() {
        let e = SessionEvent::start(t0());
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v, json!({ "id": e.id, "kind": "start", "at": "2026-11-16T09:00:00Z" }));
        assert!(e.id.len() <= 64 && e.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn stop_serializes_reason_and_note_in_wire_format() {
        let e = SessionEvent::stop(t0(), StopReason::EndOfDay, Some("  Shipped the picker.\n "));
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(
            v,
            json!({ "id": e.id, "kind": "stop", "at": "2026-11-16T09:00:00Z", "reason": "end_of_day", "note": "Shipped the picker." })
        );
        for (r, wire) in [(StopReason::Break, "break"), (StopReason::Idle, "idle"), (StopReason::Quit, "quit")] {
            let v = serde_json::to_value(SessionEvent::stop(t0(), r, Some("ignored"))).unwrap();
            assert_eq!(v["reason"], wire);
            assert!(v.get("note").is_none(), "a note only goes with end_of_day");
            assert_eq!(StopReason::parse(wire), Some(r));
        }
        assert_eq!(StopReason::parse("end_of_day"), Some(StopReason::EndOfDay));
        assert_eq!(StopReason::parse("lunch"), None);
        // Round trip through the on-disk queue format.
        let back: SessionEvent = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn notes_are_trimmed_bounded_and_never_empty() {
        assert_eq!(SessionEvent::stop(t0(), StopReason::EndOfDay, Some("   ")).note, None);
        assert_eq!(SessionEvent::stop(t0(), StopReason::EndOfDay, None).note, None);
        let long = "ç".repeat(MAX_NOTE_CHARS + 50);
        let n = SessionEvent::stop(t0(), StopReason::EndOfDay, Some(&long)).note.unwrap();
        assert_eq!(n.chars().count(), MAX_NOTE_CHARS);
    }

    #[test]
    fn queue_is_bounded_and_keeps_the_newest() {
        let mut q = Vec::new();
        for i in 0..(MAX_EVENTS as i64 + 5) {
            push_bounded(&mut q, SessionEvent::start(t0() + Duration::minutes(i)));
        }
        assert_eq!(q.len(), MAX_EVENTS);
        assert_eq!(q[0].at, t0() + Duration::minutes(5));
    }

    #[test]
    fn events_older_than_thirty_days_are_pruned() {
        let now = t0();
        let mut q = vec![
            SessionEvent::start(now - Duration::days(MAX_AGE_DAYS) - Duration::seconds(1)),
            SessionEvent::start(now - Duration::days(MAX_AGE_DAYS) + Duration::seconds(1)),
            SessionEvent::stop(now, StopReason::Idle, None),
        ];
        assert!(prune_old(&mut q, now));
        assert_eq!(q.len(), 2);
        assert!(!prune_old(&mut q, now));
    }
}
