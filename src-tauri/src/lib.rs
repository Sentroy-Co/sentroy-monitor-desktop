//! Sentroy Monitor desktop agent.
//!
//! One background thread samples activity once a second while the timer runs
//! and cuts one-minute samples into an offline queue; another uploads the
//! queue (and queued screenshots). The UI (ui/) talks to this core through the
//! commands below and re-renders on the `state` event.

mod activity;
mod api;
mod os;
mod screenshots;
mod sessions;
mod sites;
mod store;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Local, TimeZone, Utc};
use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_notification::{NotificationExt, PermissionState};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;

use activity::{activity_percent, Accumulator, Sample, SAMPLE_SECS};
use api::{Api, ApiError, DeviceStart, Policy, ScreenshotPolicy, SessionsUpload, ShotUpload};
use screenshots::{plan_offsets, ShotQueue, WINDOW_SECS};
use sessions::{SessionEvent, StopReason};
use store::{push_bounded, Settings, Store, TodayApp, BARS};

/// Rows in the "today by app" list.
const TODAY_APPS: usize = 8;

const UPLOAD_EVERY: Duration = Duration::from_secs(30);
const UPLOAD_BATCH: usize = 200;
/// Screenshots uploaded per uploader round.
const SHOT_BATCH: usize = 10;
/// After a 404 from `/api/agent/sessions` (older server), wait this long
/// before asking again; the events stay queued meanwhile.
const SESSIONS_RETRY_AFTER: chrono::Duration = chrono::Duration::hours(6);
const POLICY_EVERY: Duration = Duration::from_secs(15 * 60);
const UPDATE_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

struct Tracking {
    acc: Accumulator,
    started_at: DateTime<Utc>,
    session_seconds: u64,
    /// Recent per-second activity for the live percentage (last 10 minutes).
    recent: std::collections::VecDeque<(bool, bool)>,
    /// Session second at which the current screenshot window began.
    shot_window_start: u64,
    /// Remaining screenshot moments (seconds into the window), ascending.
    shot_offsets: Vec<u64>,
}

impl Tracking {
    fn new(now: DateTime<Utc>, acc: Accumulator, policy: &ScreenshotPolicy) -> Self {
        Tracking {
            acc,
            started_at: now,
            session_seconds: 0,
            recent: Default::default(),
            shot_window_start: 0,
            shot_offsets: shot_plan(policy),
        }
    }

    /// Called after each tracked second: whether a screenshot is due now.
    fn shot_due(&mut self, policy: &ScreenshotPolicy) -> bool {
        if self.session_seconds - self.shot_window_start >= WINDOW_SECS {
            self.shot_window_start += WINDOW_SECS;
            self.shot_offsets = shot_plan(policy);
        }
        if !policy.enabled {
            self.shot_offsets.clear();
            return false;
        }
        let pos = self.session_seconds - self.shot_window_start;
        if self.shot_offsets.first().is_some_and(|&o| pos >= o) {
            self.shot_offsets.remove(0);
            return true;
        }
        false
    }
}

fn shot_plan(policy: &ScreenshotPolicy) -> Vec<u64> {
    if policy.enabled {
        plan_offsets(policy.per_ten_minutes, uuid::Uuid::new_v4().as_u128())
    } else {
        Vec::new()
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct UpdateInfo {
    version: String,
    notes: Option<String>,
}

#[derive(Default)]
struct Inner {
    settings: Settings,
    token: Option<String>,
    queue: Vec<Sample>,
    /// Clock-in/out events waiting for upload (see `sessions`).
    sessions: Vec<SessionEvent>,
    /// Set when the server has no sessions endpoint: no upload before then.
    sessions_retry_at: Option<DateTime<Utc>>,
    tracking: Option<Tracking>,
    login: Option<DeviceStart>,
    online: bool,
    last_sync: Option<DateTime<Utc>>,
    notice: Option<String>,
    shots_queued: usize,
    update: Option<UpdateInfo>,
    updating: bool,
}

struct Shared {
    inner: Mutex<Inner>,
    store: Store,
    shots: ShotQueue,
    nudge: Mutex<mpsc::Sender<()>>,
    capturing: AtomicBool,
    pending_update: Mutex<Option<tauri_plugin_updater::Update>>,
}

impl Shared {
    fn nudge(&self) {
        let _ = self.nudge.lock().map(|n| n.send(()));
    }
}

type AppState = Arc<Shared>;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct BarView {
    start: DateTime<Utc>,
    percent: Option<u32>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    version: &'static str,
    platform: &'static str,
    server_url: String,
    /// The Sentroy cloud server (the sign-in screen's default choice).
    default_server: &'static str,
    consent_accepted: bool,
    signed_in: bool,
    user_name: Option<String>,
    company_name: Option<String>,
    login: Option<DeviceStart>,
    tracking: bool,
    session_seconds: u64,
    today_seconds: u64,
    today_apps: Vec<TodayApp>,
    bars: Vec<BarView>,
    activity_percent: Option<u32>,
    queued: usize,
    screenshots_queued: usize,
    policy: Policy,
    online: bool,
    last_sync: Option<DateTime<Utc>>,
    notice: Option<String>,
    update: Option<UpdateInfo>,
    updating: bool,
}

fn local_day() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

fn slot_of(t: DateTime<Utc>) -> i64 {
    t.timestamp().div_euclid(600)
}

/// Today's per-app totals, including the running (not yet finished) sample;
/// the largest first, at most `TODAY_APPS`.
fn today_apps(i: &Inner, is_today: bool) -> Vec<TodayApp> {
    let mut list: Vec<TodayApp> = if is_today { i.settings.today_apps.clone() } else { Vec::new() };
    if let Some(t) = &i.tracking {
        for (name, secs) in t.acc.app_seconds() {
            match list.iter_mut().find(|x| x.name == name) {
                Some(x) => x.seconds += secs as u64,
                None => list.push(TodayApp { name: name.to_string(), seconds: secs as u64 }),
            }
        }
    }
    list.sort_by(|a, b| b.seconds.cmp(&a.seconds).then_with(|| a.name.cmp(&b.name)));
    list.truncate(TODAY_APPS);
    list
}

fn bar_views(i: &Inner, now: DateTime<Utc>) -> Vec<BarView> {
    let current = slot_of(now);
    (0..BARS as i64)
        .map(|k| {
            let slot = current - (BARS as i64 - 1) + k;
            let percent = i
                .settings
                .bars
                .iter()
                .find(|b| b.slot == slot && b.tracked > 0)
                .map(|b| activity_percent(b.active, b.tracked, b.idle));
            BarView { start: Utc.timestamp_opt(slot * 600, 0).single().unwrap_or(now), percent }
        })
        .collect()
}

fn snapshot(i: &Inner) -> Snapshot {
    let is_today = i.settings.today.as_deref() == Some(&local_day());
    let today_seconds = if is_today { i.settings.today_seconds } else { 0 };
    Snapshot {
        version: env!("CARGO_PKG_VERSION"),
        platform: std::env::consts::OS,
        server_url: i.settings.server_url.clone(),
        default_server: store::DEFAULT_SERVER,
        consent_accepted: i.settings.consent_accepted,
        signed_in: i.token.is_some(),
        user_name: i.settings.user_name.clone(),
        company_name: i.settings.company_name.clone(),
        login: i.login.clone(),
        tracking: i.tracking.is_some(),
        session_seconds: i.tracking.as_ref().map(|t| t.session_seconds).unwrap_or(0),
        today_seconds,
        today_apps: today_apps(i, is_today),
        bars: bar_views(i, Utc::now()),
        activity_percent: i.tracking.as_ref().map(|t| {
            let tracked = t.recent.len() as u32;
            let active = t.recent.iter().filter(|(a, _)| *a).count() as u32;
            let idle = t.recent.iter().filter(|(_, i)| *i).count() as u32;
            activity_percent(active, tracked, idle)
        }),
        queued: i.queue.len(),
        screenshots_queued: i.shots_queued,
        policy: i.settings.policy.clone(),
        online: i.online,
        last_sync: i.last_sync,
        notice: i.notice.clone(),
        update: i.update.clone(),
        updating: i.updating,
    }
}

fn hms(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

fn emit(app: &AppHandle, i: &Inner) {
    let _ = app.emit("state", snapshot(i));
    if let Some(tray) = app.try_state::<TrayIcon>() {
        let title = i.tracking.as_ref().map(|t| hms(t.session_seconds));
        let _ = tray.set_title(title.as_deref());
    }
}

/// Closes the current sample (if any) into the queue and today's app totals.
fn flush_sample(shared: &Shared, i: &mut Inner, now: DateTime<Utc>) {
    if let Some(t) = i.tracking.as_mut() {
        let acc = std::mem::replace(&mut t.acc, Accumulator::new(now));
        if let Some(s) = acc.finish(now) {
            i.settings.roll_day(&local_day());
            for app in &s.apps {
                i.settings.add_app_time(&app.name, app.seconds as u64);
            }
            push_bounded(&mut i.queue, s);
            shared.store.save_queue(&i.queue);
            shared.store.save_settings(&i.settings);
        }
    }
}

/// Queues a clock-in/out event and persists the queue.
fn enqueue_session(shared: &Shared, i: &mut Inner, e: SessionEvent) {
    sessions::prune_old(&mut i.sessions, e.at);
    sessions::push_bounded(&mut i.sessions, e);
    shared.store.save_sessions(&i.sessions);
}

/// Starts the timer (when it is not running) and queues the `start` event.
fn start_tracking_locked(shared: &Shared, i: &mut Inner, now: DateTime<Utc>) {
    if i.tracking.is_none() {
        i.tracking = Some(Tracking::new(now, Accumulator::new(now), &i.settings.policy.screenshots));
        i.notice = None;
        enqueue_session(shared, i, SessionEvent::start(now));
    }
}

/// Stops a running timer: flushes the open sample and queues the `stop` event
/// with its reason (and, for the end of the day, the note). The uploader is
/// woken so both go out right away when online.
fn stop_tracking_locked(shared: &Shared, i: &mut Inner, reason: StopReason, note: Option<&str>) {
    if i.tracking.is_none() {
        return;
    }
    let now = Utc::now();
    flush_sample(shared, i, now);
    i.tracking = None;
    enqueue_session(shared, i, SessionEvent::stop(now, reason, note));
    shared.store.save_settings(&i.settings);
    shared.nudge();
}

fn sign_out_locked(shared: &Shared, i: &mut Inner) {
    stop_tracking_locked(shared, i, StopReason::Quit, None);
    Store::delete_token(&i.settings.server_url);
    i.token = None;
    i.settings.user_name = None;
    i.settings.company_name = None;
    i.settings.company_slug = None;
    i.settings.policy = Policy::default();
    shared.store.save_settings(&i.settings);
    // Screenshots belong to the signed-out account: never send them elsewhere.
    for shot in shared.shots.list() {
        shared.shots.remove(&shot);
    }
    i.shots_queued = 0;
    // Clock-in/out events belong to this device registration, which is gone:
    // whatever could not be delivered before signing out is dropped too.
    i.sessions.clear();
    i.sessions_retry_at = None;
    shared.store.save_sessions(&i.sessions);
}

/// Whether the Screen Recording permission the policy needs is missing.
fn screen_permission_missing(policy: &Policy) -> bool {
    policy.screenshots.enabled && os::screen_capture_supported() && !os::screen_capture_granted()
}

fn ticker(app: AppHandle, shared: AppState) {
    loop {
        thread::sleep(Duration::from_secs(1));
        // Ask the OS outside the lock.
        let since = os::seconds_since_input();
        let since_input = since.unwrap_or(activity::UNKNOWN_INPUT_SECS);
        let front = os::frontmost_app();
        let mut i = shared.inner.lock().unwrap();
        let now = Utc::now();
        let mut shot = None;
        let policy = i.settings.policy.clone();
        let site = sites::current(policy.sites && i.tracking.is_some(), front.as_deref());
        if let Some(t) = i.tracking.as_mut() {
            let active = since_input < activity::ACTIVE_WINDOW_SECS;
            let idle = since_input >= activity::IDLE_THRESHOLD_SECS;
            t.acc.tick(since_input, front.as_deref(), site.as_deref());
            t.session_seconds += 1;
            t.recent.push_back((active, idle));
            while t.recent.len() > 600 {
                t.recent.pop_front();
            }
            if t.shot_due(&policy.screenshots) {
                shot = Some(policy.screenshots.blur);
            }
            let cut = (now - t.started_at).num_seconds() > 0 && t.acc.tracked_seconds() as i64 >= SAMPLE_SECS;
            i.settings.roll_day(&local_day());
            i.settings.today_seconds += 1;
            i.settings.add_bar_second(slot_of(now), active, idle);
            if cut {
                flush_sample(&shared, &mut i, now);
            }
            if since.is_some() && since_input >= policy.idle_stop_minutes as f64 * 60.0 {
                stop_tracking_locked(&shared, &mut i, StopReason::Idle, None);
                i.notice = Some("autoStopped".into());
                shot = None;
            }
        }
        emit(&app, &i);
        drop(i);
        if let Some(blur) = shot {
            let (app, shared) = (app.clone(), shared.clone());
            thread::spawn(move || take_screenshot(&app, &shared, blur));
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ShotEvent {
    taken_at: DateTime<Utc>,
    blurred: bool,
}

/// Captures, processes and queues one screenshot. Never runs without the
/// permission or after the timer stopped.
fn take_screenshot(app: &AppHandle, shared: &Shared, blur: bool) {
    if !os::screen_capture_granted() || shared.capturing.swap(true, Ordering::SeqCst) {
        return;
    }
    let result = (|| {
        let taken_at = Utc::now();
        let img = os::capture_displays()?;
        // Blur (when asked) happens here, before anything is written to disk.
        let (jpeg, w, h) = screenshots::process(img, blur)?;
        if shared.inner.lock().unwrap().tracking.is_none() {
            return Err("timer stopped".to_string());
        }
        shared.shots.save(&jpeg, taken_at, w, h).map_err(|e| e.to_string())?;
        Ok(taken_at)
    })();
    shared.capturing.store(false, Ordering::SeqCst);
    match result {
        Ok(taken_at) => {
            let mut i = shared.inner.lock().unwrap();
            i.shots_queued = shared.shots.len();
            emit(app, &i);
            drop(i);
            let _ = app.emit("screenshot", ShotEvent { taken_at, blurred: blur });
            shared.nudge();
        }
        Err(e) => eprintln!("screenshot skipped: {e}"),
    }
}

fn upload_samples(shared: &Shared, server: &str, token: &str) {
    let batch: Vec<Sample> = {
        let i = shared.inner.lock().unwrap();
        if i.queue.is_empty() {
            return;
        }
        i.queue.iter().take(UPLOAD_BATCH).cloned().collect()
    };
    let result = Api::new(server).upload(token, &batch);
    let mut i = shared.inner.lock().unwrap();
    match result {
        Ok(()) => {
            let sent: std::collections::HashSet<&str> = batch.iter().map(|s| s.id.as_str()).collect();
            i.queue.retain(|s| !sent.contains(s.id.as_str()));
            shared.store.save_queue(&i.queue);
            i.online = true;
            i.last_sync = Some(Utc::now());
            if !i.queue.is_empty() {
                shared.nudge();
            }
        }
        Err(ApiError::Unauthorized) => {
            sign_out_locked(shared, &mut i);
            i.notice = Some("signedOut".into());
        }
        Err(ApiError::Rejected(msg)) => {
            // A sample the server will never accept must not block the queue:
            // drop the one it names (`samples[3]: …`), or the batch if unnamed.
            let bad: Vec<String> = match rejected_index(&msg, "samples").and_then(|n| batch.get(n)) {
                Some(s) => vec![s.id.clone()],
                None => batch.iter().map(|s| s.id.clone()).collect(),
            };
            i.queue.retain(|s| !bad.contains(&s.id));
            shared.store.save_queue(&i.queue);
            shared.nudge();
        }
        Err(_) => i.online = false,
    }
}

fn upload_shots(shared: &Shared, server: &str, token: &str) {
    let api = Api::new(server);
    for shot in shared.shots.list().into_iter().take(SHOT_BATCH) {
        if shared.inner.lock().unwrap().token.as_deref() != Some(token) {
            return;
        }
        let Ok(jpeg) = std::fs::read(&shot.path) else {
            shared.shots.remove(&shot);
            continue;
        };
        let outcome = api.upload_screenshot(token, &shot.id, shot.taken_at, shot.width, shot.height, jpeg);
        eprintln!("screenshot {}: {outcome:?}", shot.id);
        match outcome {
            ShotUpload::Done | ShotUpload::Drop => {
                shared.shots.remove(&shot);
                shared.inner.lock().unwrap().online = true;
            }
            ShotUpload::Retry => {
                shared.inner.lock().unwrap().online = false;
                break;
            }
            ShotUpload::Unauthorized => {
                let mut i = shared.inner.lock().unwrap();
                sign_out_locked(shared, &mut i);
                i.notice = Some("signedOut".into());
                break;
            }
        }
    }
    shared.inner.lock().unwrap().shots_queued = shared.shots.len();
}

/// Applies the outcome of uploading `batch` (the oldest queued events) to the
/// queue. Returns true when more is queued and could go out right away.
fn settle_sessions(i: &mut Inner, batch: &[SessionEvent], outcome: SessionsUpload, now: DateTime<Utc>) -> bool {
    match outcome {
        SessionsUpload::Done => {
            let sent: std::collections::HashSet<&str> = batch.iter().map(|e| e.id.as_str()).collect();
            i.sessions.retain(|e| !sent.contains(e.id.as_str()));
            i.sessions_retry_at = None;
            i.online = true;
            i.last_sync = Some(now);
            !i.sessions.is_empty()
        }
        SessionsUpload::NotAvailable => {
            // An older server: keep them (pruned after 30 days), ask again later.
            i.sessions_retry_at = Some(now + SESSIONS_RETRY_AFTER);
            i.online = true;
            false
        }
        SessionsUpload::Rejected(msg) => {
            // Never let an event the server refuses block the queue.
            let bad: Vec<String> = match rejected_index(&msg, "events").and_then(|n| batch.get(n)) {
                Some(e) => vec![e.id.clone()],
                None => batch.iter().map(|e| e.id.clone()).collect(),
            };
            i.sessions.retain(|e| !bad.contains(&e.id));
            !i.sessions.is_empty()
        }
        SessionsUpload::Retry => {
            i.online = false;
            false
        }
        // Handled by the caller (sign-out).
        SessionsUpload::Unauthorized => false,
    }
}

/// Uploads the oldest queued clock-in/out events. Returns true when more are
/// waiting and could be sent right away.
fn upload_sessions(shared: &Shared, server: &str, token: &str) -> bool {
    let now = Utc::now();
    let batch: Vec<SessionEvent> = {
        let mut i = shared.inner.lock().unwrap();
        if sessions::prune_old(&mut i.sessions, now) {
            shared.store.save_sessions(&i.sessions);
        }
        if i.sessions.is_empty() || i.sessions_retry_at.is_some_and(|t| now < t) {
            return false;
        }
        i.sessions.iter().take(sessions::UPLOAD_BATCH).cloned().collect()
    };
    let outcome = Api::new(server).upload_sessions(token, &batch);
    let mut i = shared.inner.lock().unwrap();
    if i.token.as_deref() != Some(token) {
        return false;
    }
    if outcome == SessionsUpload::Unauthorized {
        sign_out_locked(shared, &mut i);
        i.notice = Some("signedOut".into());
        return false;
    }
    let more = settle_sessions(&mut i, &batch, outcome, now);
    shared.store.save_sessions(&i.sessions);
    more
}

fn uploader(app: AppHandle, shared: AppState, rx: mpsc::Receiver<()>) {
    loop {
        let _ = rx.recv_timeout(UPLOAD_EVERY);
        let (server, token) = {
            let i = shared.inner.lock().unwrap();
            match &i.token {
                Some(t) => (i.settings.server_url.clone(), t.clone()),
                None => continue,
            }
        };
        upload_samples(&shared, &server, &token);
        if upload_sessions(&shared, &server, &token) {
            shared.nudge();
        }
        upload_shots(&shared, &server, &token);
        emit(&app, &shared.inner.lock().unwrap());
    }
}

/// Synchronously tries to deliver everything queued (before sign-out, an
/// update or a restart).
fn deliver_now(shared: &Shared) {
    let (server, token) = {
        let i = shared.inner.lock().unwrap();
        match &i.token {
            Some(t) => (i.settings.server_url.clone(), t.clone()),
            None => return,
        }
    };
    let api = Api::new(&server);
    let batch: Vec<Sample> = shared.inner.lock().unwrap().queue.clone();
    for chunk in batch.chunks(UPLOAD_BATCH) {
        if api.upload(&token, chunk).is_err() {
            break;
        }
        let ids: std::collections::HashSet<String> = chunk.iter().map(|s| s.id.clone()).collect();
        let mut i = shared.inner.lock().unwrap();
        i.queue.retain(|s| !ids.contains(&s.id));
        shared.store.save_queue(&i.queue);
    }
    for _ in 0..sessions::MAX_EVENTS.div_ceil(sessions::UPLOAD_BATCH) {
        if !upload_sessions(shared, &server, &token) {
            break;
        }
    }
    upload_shots(shared, &server, &token);
}

/// Index from a server validation message like `invalid_samples samples[3]: bad id`
/// (`field` = "samples") or `invalid_events events[1]: …` (`field` = "events").
fn rejected_index(msg: &str, field: &str) -> Option<usize> {
    let key = format!("{field}[");
    let rest = &msg[msg.find(&key)? + key.len()..];
    rest[..rest.find(']')?].parse().ok()
}

fn refresh_account(app: &AppHandle, shared: &Shared) {
    let (server, token) = {
        let i = shared.inner.lock().unwrap();
        match &i.token {
            Some(t) => (i.settings.server_url.clone(), t.clone()),
            None => return,
        }
    };
    let me = Api::new(&server).me(&token);
    let mut i = shared.inner.lock().unwrap();
    match me {
        Err(ApiError::Unauthorized) => sign_out_locked(shared, &mut i),
        Ok(me) => {
            i.settings.user_name = Some(me.user.name);
            i.settings.company_name = me.company.as_ref().map(|c| c.name.clone());
            i.settings.company_slug = me.company.map(|c| c.slug);
            i.settings.policy = me.policy.unwrap_or_default().sanitized();
            i.online = true;
            shared.store.save_settings(&i.settings);
        }
        Err(_) => i.online = false,
    }
    emit(app, &i);
}

fn computer_name() -> String {
    os::computer_name().map(|n| n.chars().take(80).collect()).unwrap_or_else(|| "Desktop".into())
}

async fn check_update(app: &AppHandle) {
    let Ok(updater) = app.updater() else { return };
    match updater.check().await {
        Ok(Some(update)) => {
            let shared: State<'_, AppState> = app.state();
            let info = UpdateInfo { version: update.version.clone(), notes: update.body.clone() };
            *shared.pending_update.lock().unwrap() = Some(update);
            let mut i = shared.inner.lock().unwrap();
            i.update = Some(info);
            emit(app, &i);
        }
        Ok(None) => {}
        Err(e) => eprintln!("update check failed: {e}"),
    }
}

// ── commands ────────────────────────────────────────────────────────────────

#[tauri::command]
fn get_state(state: State<'_, AppState>) -> Snapshot {
    snapshot(&state.inner.lock().unwrap())
}

#[tauri::command]
fn accept_consent(app: AppHandle, state: State<'_, AppState>) {
    let mut i = state.inner.lock().unwrap();
    i.settings.consent_accepted = true;
    state.store.save_settings(&i.settings);
    emit(&app, &i);
}

#[tauri::command]
fn dismiss_notice(app: AppHandle, state: State<'_, AppState>) {
    let mut i = state.inner.lock().unwrap();
    i.notice = None;
    emit(&app, &i);
}

#[tauri::command]
fn set_server(app: AppHandle, state: State<'_, AppState>, url: String) -> Result<(), String> {
    let url = url.trim().trim_end_matches('/').to_string();
    let ok = url.starts_with("https://") || url.starts_with("http://localhost") || url.starts_with("http://127.0.0.1");
    if !ok || url.len() > 200 {
        return Err("invalidServer".into());
    }
    let mut i = state.inner.lock().unwrap();
    if i.token.is_some() {
        return Err("signOutFirst".into());
    }
    i.settings.server_url = url;
    state.store.save_settings(&i.settings);
    emit(&app, &i);
    Ok(())
}

#[tauri::command]
async fn start_login(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let shared = state.inner().clone();
    let server = shared.inner.lock().unwrap().settings.server_url.clone();
    let start = tauri::async_runtime::spawn_blocking(move || Api::new(&server).device_start(&computer_name(), env!("CARGO_PKG_VERSION")))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| match e {
            ApiError::Offline(_) => "offline".to_string(),
            other => other.to_string(),
        })?;
    let _ = app.opener().open_url(&start.verification_uri_complete, None::<&str>);
    {
        let mut i = shared.inner.lock().unwrap();
        i.login = Some(start.clone());
        emit(&app, &i);
    }
    // Poll until approved, denied, expired or cancelled.
    let app2 = app.clone();
    thread::spawn(move || {
        let server = shared.inner.lock().unwrap().settings.server_url.clone();
        let api = Api::new(&server);
        let deadline = std::time::Instant::now() + Duration::from_secs(start.expires_in);
        let mut interval = start.interval.max(2);
        loop {
            thread::sleep(Duration::from_secs(interval));
            let still = shared.inner.lock().unwrap().login.as_ref().map(|l| l.device_code == start.device_code).unwrap_or(false);
            if !still {
                return; // cancelled or replaced
            }
            if std::time::Instant::now() > deadline {
                let mut i = shared.inner.lock().unwrap();
                i.login = None;
                i.notice = Some("loginExpired".into());
                emit(&app2, &i);
                return;
            }
            match api.device_token(&start.device_code) {
                Ok(token) => {
                    {
                        let mut i = shared.inner.lock().unwrap();
                        Store::save_token(&server, &token);
                        i.token = Some(token);
                        i.login = None;
                        i.notice = None;
                        emit(&app2, &i);
                    }
                    refresh_account(&app2, &shared);
                    show_main(&app2);
                    return;
                }
                Err(ApiError::Pending) => {}
                Err(ApiError::Offline(_)) => interval = (interval + 2).min(15),
                Err(e) => {
                    let mut i = shared.inner.lock().unwrap();
                    i.login = None;
                    i.notice = Some(if matches!(e, ApiError::Denied) { "loginDenied" } else { "loginExpired" }.into());
                    emit(&app2, &i);
                    return;
                }
            }
        }
    });
    Ok(())
}

#[tauri::command]
fn cancel_login(app: AppHandle, state: State<'_, AppState>) {
    let mut i = state.inner.lock().unwrap();
    i.login = None;
    emit(&app, &i);
}

#[tauri::command]
fn open_login_page(app: AppHandle, state: State<'_, AppState>) {
    let url = state.inner.lock().unwrap().login.as_ref().map(|l| l.verification_uri_complete.clone());
    if let Some(url) = url {
        let _ = app.opener().open_url(url, None::<&str>);
    }
}

#[tauri::command]
async fn logout(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let shared = state.inner().clone();
    let (server, token) = {
        let mut i = shared.inner.lock().unwrap();
        stop_tracking_locked(&shared, &mut i, StopReason::Quit, None);
        (i.settings.server_url.clone(), i.token.clone())
    };
    // Try to deliver queued time and screenshots before the token goes away.
    if let Some(token) = token {
        let s2 = shared.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || {
            deliver_now(&s2);
            let _ = Api::new(&server).logout(&token);
        })
        .await;
    }
    let mut i = shared.inner.lock().unwrap();
    sign_out_locked(&shared, &mut i);
    emit(&app, &i);
    Ok(())
}

#[tauri::command]
async fn refresh(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || refresh_account(&app, &shared)).await.map_err(|e| e.to_string())
}

#[tauri::command]
fn start_tracking(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let mut i = state.inner.lock().unwrap();
    if i.token.is_none() {
        return Err("signedOut".into());
    }
    if !i.settings.consent_accepted {
        return Err("consentRequired".into());
    }
    if screen_permission_missing(&i.settings.policy) {
        return Err("permissionsRequired".into());
    }
    start_tracking_locked(&state, &mut i, Utc::now());
    emit(&app, &i);
    Ok(())
}

/// Stops the timer from the stop dialog: `reason` is `end_of_day` (with an
/// optional `note`) or `break`; anything else counts as the end of the day.
#[tauri::command]
fn stop_tracking(app: AppHandle, state: State<'_, AppState>, reason: Option<String>, note: Option<String>) {
    let reason = reason.as_deref().and_then(StopReason::parse).unwrap_or(StopReason::EndOfDay);
    let mut i = state.inner.lock().unwrap();
    stop_tracking_locked(&state, &mut i, reason, note.as_deref());
    emit(&app, &i);
}

#[tauri::command]
fn open_dashboard(app: AppHandle, state: State<'_, AppState>, lang: String) {
    let i = state.inner.lock().unwrap();
    let lang = if ["en", "tr", "ru", "zh", "es", "id"].contains(&lang.as_str()) { lang } else { "en".into() };
    let url = match &i.settings.company_slug {
        Some(slug) => format!("{}/{}/d/{}/timeline", i.settings.server_url, lang, slug),
        None => format!("{}/{}", i.settings.server_url, lang),
    };
    let _ = app.opener().open_url(url, None::<&str>);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PermissionItem {
    kind: &'static str,
    /// Tracking cannot start while a required permission is missing.
    required: bool,
    granted: bool,
    /// Whether `open_permission_settings` can open a settings page for it.
    has_settings: bool,
    /// macOS only notices this grant after the app restarts.
    needs_restart: bool,
}

#[tauri::command]
fn permissions_status(app: AppHandle, state: State<'_, AppState>) -> Vec<PermissionItem> {
    let policy = state.inner.lock().unwrap().settings.policy.clone();
    let mut out = Vec::new();
    if policy.screenshots.enabled && os::screen_capture_supported() {
        out.push(PermissionItem {
            kind: "screen",
            required: true,
            granted: os::screen_capture_granted(),
            has_settings: os::permission_settings_url("screen").is_some(),
            needs_restart: cfg!(target_os = "macos"),
        });
    }
    let notifications = app.notification().permission_state().map(|p| p == PermissionState::Granted).unwrap_or(false);
    out.push(PermissionItem {
        kind: "notifications",
        required: false,
        granted: notifications,
        has_settings: os::permission_settings_url("notifications").is_some(),
        needs_restart: false,
    });
    out
}

#[tauri::command]
fn request_permission(app: AppHandle, state: State<'_, AppState>, kind: String) -> Vec<PermissionItem> {
    match kind.as_str() {
        "screen" => {
            os::request_screen_capture();
        }
        "notifications" => {
            let _ = app.notification().request_permission();
        }
        _ => {}
    }
    permissions_status(app, state)
}

#[tauri::command]
fn open_permission_settings(app: AppHandle, kind: String) {
    if let Some(url) = os::permission_settings_url(&kind) {
        let _ = app.opener().open_url(url, None::<&str>);
    }
}

/// Stops the timer, delivers what is queued and restarts (macOS needs this
/// after Screen Recording was granted).
#[tauri::command]
async fn restart_app(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let shared = state.inner().clone();
    stop_tracking_locked(&shared, &mut shared.inner.lock().unwrap(), StopReason::Quit, None);
    let _ = tauri::async_runtime::spawn_blocking(move || deliver_now(&shared)).await;
    app.restart()
}

/// Shows a system notification for a screenshot when the window is not in
/// front (the UI passes the localized text).
#[tauri::command]
fn notify_screenshot(app: AppHandle, title: String, body: String) {
    let focused = app.get_webview_window("main").and_then(|w| w.is_focused().ok()).unwrap_or(false);
    if !focused {
        let _ = app.notification().builder().title(title).body(body).show();
    }
}

#[tauri::command]
async fn install_update(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let shared = state.inner().clone();
    let update = shared.pending_update.lock().unwrap().take().ok_or("noUpdate")?;
    {
        let mut i = shared.inner.lock().unwrap();
        // Never restart under a running timer: stop and flush it first.
        stop_tracking_locked(&shared, &mut i, StopReason::Quit, None);
        i.updating = true;
        emit(&app, &i);
    }
    let s2 = shared.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || deliver_now(&s2)).await;
    match update.download_and_install(|_, _| {}, || {}).await {
        Ok(()) => app.restart(),
        Err(e) => {
            eprintln!("update failed: {e}");
            *shared.pending_update.lock().unwrap() = Some(update);
            let mut i = shared.inner.lock().unwrap();
            i.updating = false;
            i.notice = Some("updateFailed".into());
            emit(&app, &i);
            Err("updateFailed".into())
        }
    }
}

/// Tray "Start / Stop". Stopping goes through the in-app "End your day?"
/// dialog, so the window is brought up and asked to show it.
fn toggle_tracking(app: &AppHandle) {
    let state: State<'_, AppState> = app.state();
    let tracking = state.inner.lock().unwrap().tracking.is_some();
    if tracking {
        show_main(app);
        let _ = app.emit("confirm-stop", ());
    } else {
        let _ = start_tracking(app.clone(), state);
        show_main(app);
    }
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let dir = app.path().app_data_dir().expect("app data dir");
            let store = Store::new(dir.clone());
            let shots = ShotQueue::new(dir.join("screenshots"));
            let settings = store.load_settings();
            let token = Store::load_token(&settings.server_url);
            let queue = store.load_queue();
            let sessions = store.load_sessions();
            let shots_queued = shots.len();
            let (tx, rx) = mpsc::channel();
            let shared: AppState = Arc::new(Shared {
                inner: Mutex::new(Inner { settings, token, queue, sessions, shots_queued, ..Default::default() }),
                store,
                shots,
                nudge: Mutex::new(tx),
                capturing: AtomicBool::new(false),
                pending_update: Mutex::new(None),
            });
            app.manage(shared.clone());

            let show = MenuItem::with_id(app, "show", "Sentroy Monitor", true, None::<&str>)?;
            let toggle = MenuItem::with_id(app, "toggle", "Start / Stop", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, Some("CmdOrCtrl+Q"))?;
            let menu = Menu::with_items(app, &[&show, &toggle, &PredefinedMenuItem::separator(app)?, &quit])?;
            let tray = TrayIconBuilder::with_id("main")
                .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?)
                .icon_as_template(true)
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, e| match e.id.as_ref() {
                    "show" => show_main(app),
                    "toggle" => toggle_tracking(app),
                    "quit" => {
                        let state: State<'_, AppState> = app.state();
                        stop_tracking_locked(&state, &mut state.inner.lock().unwrap(), StopReason::Quit, None);
                        // Give the uploader one chance to send the last minute.
                        thread::sleep(Duration::from_millis(1500));
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;
            app.manage(tray);

            let h = app.handle().clone();
            let s = shared.clone();
            thread::spawn(move || ticker(h, s));
            let h = app.handle().clone();
            let s = shared.clone();
            thread::spawn(move || uploader(h, s, rx));
            // Account and policy: now and every 15 minutes.
            let h = app.handle().clone();
            let s = shared.clone();
            thread::spawn(move || loop {
                refresh_account(&h, &s);
                thread::sleep(POLICY_EVERY);
            });
            // Updates: now and every 6 hours (never in debug builds).
            if !cfg!(debug_assertions) {
                let h = app.handle().clone();
                thread::spawn(move || loop {
                    tauri::async_runtime::block_on(check_update(&h));
                    thread::sleep(UPDATE_EVERY);
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps the agent (and the timer) running in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            accept_consent,
            dismiss_notice,
            set_server,
            start_login,
            cancel_login,
            open_login_page,
            logout,
            refresh,
            start_tracking,
            stop_tracking,
            open_dashboard,
            permissions_status,
            request_permission,
            open_permission_settings,
            restart_app,
            notify_screenshot,
            install_update,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Sentroy Monitor")
        .run(|_app, _event| {
            // Quitting by any other way than the tray (OS shutdown, Dock menu):
            // the running timer still ends with a `quit` stop event on disk.
            if let tauri::RunEvent::Exit = _event {
                if let Some(state) = _app.try_state::<AppState>() {
                    stop_tracking_locked(&state, &mut state.inner.lock().unwrap(), StopReason::Quit, None);
                }
            }
            // Clicking the Dock icon re-opens the window (macOS only event).
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = _event {
                show_main(_app);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rejected_sample_index() {
        assert_eq!(rejected_index("invalid_samples samples[3]: bad id", "samples"), Some(3));
        assert_eq!(rejected_index("invalid_samples samples must be an array", "samples"), None);
        assert_eq!(rejected_index("samples[x]", "samples"), None);
        assert_eq!(rejected_index("invalid_events events[1]: bad kind", "events"), Some(1));
        assert_eq!(rejected_index("invalid_events events[1]: bad kind", "samples"), None);
    }

    #[test]
    fn clock_format() {
        assert_eq!(hms(3725), "01:02:05");
    }

    fn tracking(policy: &ScreenshotPolicy) -> Tracking {
        let now = Utc::now();
        Tracking::new(now, Accumulator::new(now), policy)
    }

    #[test]
    fn screenshots_follow_the_policy_per_ten_minute_window() {
        let on = ScreenshotPolicy { enabled: true, per_ten_minutes: 2, blur: false };
        let mut t = tracking(&on);
        let mut due = Vec::new();
        for _ in 0..(3 * WINDOW_SECS) {
            t.session_seconds += 1;
            if t.shot_due(&on) {
                due.push(t.session_seconds);
            }
        }
        assert_eq!(due.len(), 6, "{due:?}");
        for w in 0..3 {
            let n = due.iter().filter(|&&s| s > w * WINDOW_SECS && s <= (w + 1) * WINDOW_SECS).count();
            assert_eq!(n, 2, "window {w}: {due:?}");
        }
        // Disabled: never, even with a plan left over.
        let off = ScreenshotPolicy::default();
        let mut t = tracking(&on);
        for _ in 0..WINDOW_SECS * 2 {
            t.session_seconds += 1;
            assert!(!t.shot_due(&off));
        }
    }

    #[test]
    fn today_apps_include_the_running_sample_largest_first() {
        let mut i = Inner::default();
        i.settings.today_apps = vec![TodayApp { name: "Figma".into(), seconds: 120 }, TodayApp { name: "Mail".into(), seconds: 30 }];
        let mut t = tracking(&ScreenshotPolicy::default());
        for _ in 0..50 {
            t.acc.tick(0.0, Some("Mail"), None);
        }
        t.acc.tick(0.0, Some("Slack"), None);
        i.tracking = Some(t);
        let names = |l: Vec<TodayApp>| l.into_iter().map(|a| (a.name, a.seconds)).collect::<Vec<_>>();
        assert_eq!(names(today_apps(&i, true)), vec![("Figma".into(), 120), ("Mail".into(), 80), ("Slack".into(), 1)]);
        // A stored list from another day does not count.
        assert_eq!(names(today_apps(&i, false)), vec![("Mail".into(), 50), ("Slack".into(), 1)]);
        i.tracking = None;
        i.settings.today_apps = (0..20).map(|k| TodayApp { name: format!("App {k}"), seconds: k }).collect();
        let top = today_apps(&i, true);
        assert_eq!(top.len(), TODAY_APPS);
        assert_eq!(top[0].seconds, 19);
    }

    fn shared_in(dir: &std::path::Path) -> (Shared, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let shared = Shared {
            inner: Mutex::new(Inner::default()),
            store: Store::new(dir.to_path_buf()),
            shots: ShotQueue::new(dir.join("screenshots")),
            nudge: Mutex::new(tx),
            capturing: AtomicBool::new(false),
            pending_update: Mutex::new(None),
        };
        (shared, rx)
    }

    fn temp_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("sm-lib-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn start_and_stop_queue_session_events_on_disk() {
        let dir = temp_dir();
        let (shared, rx) = shared_in(&dir);
        let mut i = shared.inner.lock().unwrap();
        // Stopping a timer that is not running records nothing.
        stop_tracking_locked(&shared, &mut i, StopReason::EndOfDay, Some("x"));
        assert!(i.sessions.is_empty());

        start_tracking_locked(&shared, &mut i, Utc::now());
        start_tracking_locked(&shared, &mut i, Utc::now()); // already running: no second start
        assert_eq!(i.sessions.len(), 1);
        assert_eq!(i.sessions[0].kind, sessions::Kind::Start);

        stop_tracking_locked(&shared, &mut i, StopReason::EndOfDay, Some(" Wrapped up the release. "));
        assert!(i.tracking.is_none());
        let stop = i.sessions.last().unwrap();
        assert_eq!((stop.kind, stop.reason), (sessions::Kind::Stop, Some(StopReason::EndOfDay)));
        assert_eq!(stop.note.as_deref(), Some("Wrapped up the release."));
        assert!(rx.try_recv().is_ok(), "the uploader is woken right after a stop");

        // A break carries no note; the idle auto-stop has its own reason.
        start_tracking_locked(&shared, &mut i, Utc::now());
        stop_tracking_locked(&shared, &mut i, StopReason::Break, Some("lunch"));
        start_tracking_locked(&shared, &mut i, Utc::now());
        stop_tracking_locked(&shared, &mut i, StopReason::Idle, None);
        let reasons: Vec<_> = i.sessions.iter().map(|e| (e.kind, e.reason, e.note.is_some())).collect();
        use sessions::Kind::{Start, Stop};
        assert_eq!(
            reasons,
            vec![
                (Start, None, false),
                (Stop, Some(StopReason::EndOfDay), true),
                (Start, None, false),
                (Stop, Some(StopReason::Break), false),
                (Start, None, false),
                (Stop, Some(StopReason::Idle), false),
            ]
        );
        // Persisted: a restart sees the same queue.
        assert_eq!(Store::new(dir.clone()).load_sessions(), i.sessions);
        drop(i);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn signing_out_while_tracking_stops_with_quit_and_drops_the_queue() {
        let dir = temp_dir();
        let (shared, _rx) = shared_in(&dir);
        let mut i = shared.inner.lock().unwrap();
        i.token = Some("t".into());
        start_tracking_locked(&shared, &mut i, Utc::now());
        stop_tracking_locked(&shared, &mut i, StopReason::Quit, None);
        assert_eq!(i.sessions.last().unwrap().reason, Some(StopReason::Quit));
        start_tracking_locked(&shared, &mut i, Utc::now());
        sign_out_locked(&shared, &mut i);
        assert!(i.tracking.is_none() && i.token.is_none());
        assert!(i.sessions.is_empty());
        assert!(Store::new(dir.clone()).load_sessions().is_empty());
        drop(i);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn session_upload_outcomes() {
        let now = Utc::now();
        let mut i = Inner {
            sessions: (0..3).map(|k| SessionEvent::start(now - chrono::Duration::minutes(3 - k))).collect(),
            ..Default::default()
        };
        let batch = i.sessions[..2].to_vec();

        // Offline: nothing changes.
        assert!(!settle_sessions(&mut i, &batch, SessionsUpload::Retry, now));
        assert_eq!((i.sessions.len(), i.online), (3, false));

        // Older server: kept, and not asked again for a while.
        assert!(!settle_sessions(&mut i, &batch, SessionsUpload::NotAvailable, now));
        assert_eq!(i.sessions.len(), 3);
        assert_eq!(i.sessions_retry_at, Some(now + SESSIONS_RETRY_AFTER));

        // Rejected naming one event: only that one goes.
        let bad = batch[1].id.clone();
        assert!(settle_sessions(&mut i, &batch, SessionsUpload::Rejected("invalid_events events[1]: x".into()), now));
        assert!(i.sessions.iter().all(|e| e.id != bad) && i.sessions.len() == 2);

        // Delivered: the batch leaves the queue, newer events stay.
        let batch = i.sessions[..1].to_vec();
        assert!(settle_sessions(&mut i, &batch, SessionsUpload::Done, now));
        assert_eq!(i.sessions.len(), 1);
        assert!(i.online && i.sessions_retry_at.is_none());

        // Rejected without an index: the whole batch is dropped.
        let batch = i.sessions.clone();
        assert!(!settle_sessions(&mut i, &batch, SessionsUpload::Rejected("invalid_events bad body".into()), now));
        assert!(i.sessions.is_empty());
    }

    #[test]
    fn bar_views_cover_the_last_hour() {
        let mut i = Inner::default();
        let now = Utc::now();
        i.settings.add_bar_second(slot_of(now), true, false);
        i.settings.add_bar_second(slot_of(now), false, false);
        let bars = bar_views(&i, now);
        assert_eq!(bars.len(), BARS);
        assert_eq!(bars[BARS - 1].percent, Some(50));
        assert!(bars[..BARS - 1].iter().all(|b| b.percent.is_none()));
    }
}
