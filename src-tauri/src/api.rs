//! HTTP client for the Sentroy Monitor agent API (`/api/agent/*`).

use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::activity::Sample;

#[derive(Debug)]
pub enum ApiError {
    /// Token missing, revoked or unknown: the agent must sign in again.
    Unauthorized,
    /// Device flow states.
    Pending,
    Denied,
    Expired,
    /// Network down, timeout, 5xx: keep data queued and retry later.
    Offline(String),
    /// The server rejected the request (4xx other than 401).
    Rejected(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unauthorized => write!(f, "unauthorized"),
            ApiError::Pending => write!(f, "authorization_pending"),
            ApiError::Denied => write!(f, "access_denied"),
            ApiError::Expired => write!(f, "expired_token"),
            ApiError::Offline(m) => write!(f, "offline: {m}"),
            ApiError::Rejected(m) => write!(f, "rejected: {m}"),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Named {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Company {
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScreenshotPolicy {
    pub enabled: bool,
    pub per_ten_minutes: u32,
    pub blur: bool,
}

impl Default for ScreenshotPolicy {
    fn default() -> Self {
        ScreenshotPolicy { enabled: false, per_ten_minutes: 1, blur: false }
    }
}

/// What the company decided for this agent (`/api/agent/me` → `policy`).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Policy {
    pub screenshots: ScreenshotPolicy,
    pub idle_stop_minutes: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Policy { screenshots: ScreenshotPolicy::default(), idle_stop_minutes: 30 }
    }
}

impl Policy {
    /// The policy with server values clamped to what the agent supports.
    pub fn sanitized(mut self) -> Self {
        self.idle_stop_minutes = self.idle_stop_minutes.clamp(5, 120);
        self.screenshots.per_ten_minutes = self.screenshots.per_ten_minutes.clamp(1, 3);
        self
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Me {
    pub device: Named,
    pub user: Named,
    pub company: Option<Company>,
    #[serde(default)]
    pub policy: Option<Policy>,
}

/// Outcome of one screenshot upload.
#[derive(Debug, PartialEq)]
pub enum ShotUpload {
    /// Stored (or already stored): delete the local file.
    Done,
    /// The server will never take it (400/403/413): drop the local file.
    Drop,
    /// Network or server trouble: keep it for later.
    Retry,
    Unauthorized,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub project_id: Option<String>,
    pub title: String,
    pub status: String,
    pub mine: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Catalog {
    pub projects: Vec<Named>,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    error: Option<String>,
    message: Option<String>,
}

fn shot_outcome(status: StatusCode) -> ShotUpload {
    match status.as_u16() {
        200..=299 => ShotUpload::Done,
        401 => ShotUpload::Unauthorized,
        400 | 403 | 413 => ShotUpload::Drop,
        _ => ShotUpload::Retry,
    }
}

pub struct Api {
    base: String,
    http: Client,
}

impl Api {
    pub fn new(base: &str) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("SentroyMonitor/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client");
        Api { base: base.trim_end_matches('/').to_string(), http }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    fn handle<T: for<'de> Deserialize<'de>>(res: reqwest::Result<reqwest::blocking::Response>) -> Result<T, ApiError> {
        let res = res.map_err(|e| ApiError::Offline(e.to_string()))?;
        let status = res.status();
        if status.is_success() {
            return res.json::<T>().map_err(|e| ApiError::Offline(format!("bad response: {e}")));
        }
        if status == StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
            return Err(ApiError::Offline(format!("HTTP {status}")));
        }
        let body: ErrorBody = res.json().unwrap_or(ErrorBody { error: None, message: None });
        match body.error.as_deref() {
            Some("authorization_pending") => Err(ApiError::Pending),
            Some("access_denied") => Err(ApiError::Denied),
            Some("expired_token") => Err(ApiError::Expired),
            other => Err(ApiError::Rejected(format!(
                "{} {}",
                other.unwrap_or("error"),
                body.message.unwrap_or_default()
            ))),
        }
    }

    pub fn device_start(&self, name: &str, version: &str) -> Result<DeviceStart, ApiError> {
        let platform = std::env::consts::OS;
        Self::handle(
            self.http
                .post(self.url("/api/agent/device/start"))
                .json(&json!({ "name": name, "platform": platform, "appVersion": version }))
                .send(),
        )
    }

    /// Returns the device token once the user approved the code.
    pub fn device_token(&self, device_code: &str) -> Result<String, ApiError> {
        #[derive(Deserialize)]
        struct Tok {
            token: String,
        }
        let t: Tok = Self::handle(
            self.http.post(self.url("/api/agent/device/token")).json(&json!({ "deviceCode": device_code })).send(),
        )?;
        Ok(t.token)
    }

    pub fn me(&self, token: &str) -> Result<Me, ApiError> {
        Self::handle(self.http.get(self.url("/api/agent/me")).bearer_auth(token).send())
    }

    pub fn catalog(&self, token: &str) -> Result<Catalog, ApiError> {
        Self::handle(self.http.get(self.url("/api/agent/projects")).bearer_auth(token).send())
    }

    pub fn create_task(&self, token: &str, project_id: Option<&str>, title: &str) -> Result<Task, ApiError> {
        Self::handle(
            self.http
                .post(self.url("/api/agent/tasks"))
                .bearer_auth(token)
                .json(&json!({ "projectId": project_id, "title": title }))
                .send(),
        )
    }

    /// Uploads samples; the server de-duplicates by sample id, so retries are safe.
    pub fn upload(&self, token: &str, samples: &[Sample]) -> Result<(), ApiError> {
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct Ack {
            accepted: u64,
        }
        let _: Ack = Self::handle(
            self.http.post(self.url("/api/agent/activity")).bearer_auth(token).json(&json!({ "samples": samples })).send(),
        )?;
        Ok(())
    }

    /// Uploads one JPEG screenshot (the id makes retries idempotent).
    pub fn upload_screenshot(
        &self,
        token: &str,
        id: &str,
        taken_at: chrono::DateTime<chrono::Utc>,
        width: u32,
        height: u32,
        jpeg: Vec<u8>,
    ) -> ShotUpload {
        let res = self
            .http
            .post(self.url("/api/agent/screenshots"))
            .bearer_auth(token)
            .query(&[
                ("id", id.to_string()),
                ("takenAt", taken_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
                ("width", width.to_string()),
                ("height", height.to_string()),
            ])
            .header(reqwest::header::CONTENT_TYPE, "image/jpeg")
            .timeout(Duration::from_secs(60))
            .body(jpeg)
            .send();
        match res {
            Err(_) => ShotUpload::Retry,
            Ok(r) => shot_outcome(r.status()),
        }
    }

    pub fn logout(&self, token: &str) -> Result<(), ApiError> {
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct Ok_ {
            ok: bool,
        }
        let _: Ok_ = Self::handle(self.http.post(self.url("/api/agent/logout")).bearer_auth(token).send())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn me_without_policy_uses_defaults() {
        let me: Me = serde_json::from_str(r#"{"device":{"id":"d","name":"D"},"user":{"id":"u","name":"U"},"company":null}"#).unwrap();
        assert!(me.policy.is_none());
        assert_eq!(Policy::default().idle_stop_minutes, 30);
    }

    #[test]
    fn parses_and_clamps_policy() {
        let me: Me = serde_json::from_str(
            r#"{"device":{"id":"d","name":"D"},"user":{"id":"u","name":"U"},"company":{"slug":"s","name":"S"},
               "policy":{"screenshots":{"enabled":true,"perTenMinutes":9,"blur":true},"idleStopMinutes":2}}"#,
        )
        .unwrap();
        let p = me.policy.unwrap().sanitized();
        assert!(p.screenshots.enabled && p.screenshots.blur);
        assert_eq!((p.screenshots.per_ten_minutes, p.idle_stop_minutes), (3, 5));
        let partial: Policy = serde_json::from_str(r#"{"screenshots":{"enabled":true}}"#).unwrap();
        assert_eq!((partial.screenshots.per_ten_minutes, partial.idle_stop_minutes), (1, 30));
    }

    #[test]
    fn screenshot_status_handling() {
        assert_eq!(shot_outcome(StatusCode::CREATED), ShotUpload::Done);
        assert_eq!(shot_outcome(StatusCode::OK), ShotUpload::Done);
        assert_eq!(shot_outcome(StatusCode::FORBIDDEN), ShotUpload::Drop);
        assert_eq!(shot_outcome(StatusCode::PAYLOAD_TOO_LARGE), ShotUpload::Drop);
        assert_eq!(shot_outcome(StatusCode::BAD_REQUEST), ShotUpload::Drop);
        assert_eq!(shot_outcome(StatusCode::UNAUTHORIZED), ShotUpload::Unauthorized);
        assert_eq!(shot_outcome(StatusCode::BAD_GATEWAY), ShotUpload::Retry);
        assert_eq!(shot_outcome(StatusCode::TOO_MANY_REQUESTS), ShotUpload::Retry);
    }
}
