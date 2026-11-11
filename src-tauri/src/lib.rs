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
use api::{Api, ApiError, Catalog, DeviceStart, Policy, ScreenshotPolicy, ShotUpload};
use screenshots::{plan_offsets, ShotQueue, WINDOW_SECS};
use store::{push_bounded, Settings, Store, TodayTask, BARS};

const UPLOAD_EVERY: Duration = Duration::from_secs(30);
const UPLOAD_BATCH: usize = 200;
/// Screenshots uploaded per uploader round.
const SHOT_BATCH: usize = 10;
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
    catalog: Option<Catalog>,
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
    consent_accepted: bool,
    signed_in: bool,
    user_name: Option<String>,
    company_name: Option<String>,
    login: Option<DeviceStart>,
    catalog: Option<Catalog>,
    project_id: Option<String>,
    task_id: Option<String>,
    tracking: bool,
    session_seconds: u64,
    today_seconds: u64,
    today_tasks: Vec<TodayTask>,
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

fn task_title(catalog: Option<&Catalog>, task_id: Option<&str>) -> Option<String> {
    let id = task_id?;
    catalog?.tasks.iter().find(|t| t.id == id).map(|t| t.title.clone())
}

/// Today's per-task totals, including the running (not yet finished) sample.
fn today_tasks(i: &Inner, is_today: bool) -> Vec<TodayTask> {
    let mut list: Vec<TodayTask> = if is_today { i.settings.today_tasks.clone() } else { Vec::new() };
    if let Some(t) = &i.tracking {
        let task_id = t.acc.task_id().map(str::to_string);
        let secs = t.acc.tracked_seconds() as u64;
        if secs > 0 {
            match list.iter_mut().find(|x| x.task_id == task_id) {
                Some(x) => x.seconds += secs,
                None => list.push(TodayTask { task_id, project_id: i.settings.project_id.clone(), title: None, seconds: secs }),
            }
        }
    }
    for x in list.iter_mut() {
        if let Some(title) = task_title(i.catalog.as_ref(), x.task_id.as_deref()) {
            x.title = Some(title);
        }
    }
    list.sort_by_key(|x| std::cmp::Reverse(x.seconds));
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
        consent_accepted: i.settings.consent_accepted,
        signed_in: i.token.is_some(),
        user_name: i.settings.user_name.clone(),
        company_name: i.settings.company_name.clone(),
        login: i.login.clone(),
        catalog: i.catalog.clone(),
        project_id: i.settings.project_id.clone(),
        task_id: i.settings.task_id.clone(),
        tracking: i.tracking.is_some(),
        session_seconds: i.tracking.as_ref().map(|t| t.session_seconds).unwrap_or(0),
        today_seconds,
        today_tasks: today_tasks(i, is_today),
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

/// Closes the current sample (if any) into the queue and today's task totals.
fn flush_sample(shared: &Shared, i: &mut Inner, now: DateTime<Utc>) {
    if let Some(t) = i.tracking.as_mut() {
        let project = i.settings.project_id.clone();
        let task = i.settings.task_id.clone();
        let acc = std::mem::replace(&mut t.acc, Accumulator::new(now, project, task));
        if let Some(s) = acc.finish(now) {
            i.settings.roll_day(&local_day());
            let secs = (s.end - s.start).num_seconds().max(0) as u64;
            let title = task_title(i.catalog.as_ref(), s.task_id.as_deref());
            i.settings.add_task_time(s.task_id.clone(), s.project_id.clone(), title, secs);
            push_bounded(&mut i.queue, s);
            shared.store.save_queue(&i.queue);
            shared.store.save_settings(&i.settings);
        }
    }
}

fn stop_tracking_locked(shared: &Shared, i: &mut Inner) {
    flush_sample(shared, i, Utc::now());
    i.tracking = None;
    shared.store.save_settings(&i.settings);
    shared.nudge();
}

fn sign_out_locked(shared: &Shared, i: &mut Inner) {
    if i.tracking.is_some() {
        stop_tracking_locked(shared, i);
    }
    Store::delete_token(&i.settings.server_url);
    i.token = None;
    i.catalog = None;
    i.settings.user_name = None;
    i.settings.company_name = None;
    i.settings.company_slug = None;
    i.settings.project_id = None;
    i.settings.task_id = None;
    i.settings.policy = Policy::default();
    shared.store.save_settings(&i.settings);
    // Screenshots belong to the signed-out account: never send them elsewhere.
    for shot in shared.shots.list() {
        shared.shots.remove(&shot);
    }
    i.shots_queued = 0;
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
        if let Some(t) = i.tracking.as_mut() {
            let active = since_input < activity::ACTIVE_WINDOW_SECS;
            let idle = since_input >= activity::IDLE_THRESHOLD_SECS;
            t.acc.tick(since_input, front.as_deref());
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
                stop_tracking_locked(&shared, &mut i);
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
        let img = os::capture_main_display()?;
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
            let bad: Vec<String> = match rejected_index(&msg).and_then(|n| batch.get(n)) {
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
            ShotUpload::Done | ShotUpload::Drop => shared.shots.remove(&shot),
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
    upload_shots(shared, &server, &token);
}

/// Index from a server validation message like `invalid_samples samples[3]: bad id`.
fn rejected_index(msg: &str) -> Option<usize> {
    let rest = &msg[msg.find("samples[")? + "samples[".len()..];
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
    let api = Api::new(&server);
    let me = api.me(&token);
    let catalog = api.catalog(&token);
    let mut i = shared.inner.lock().unwrap();
    match (me, catalog) {
        (Err(ApiError::Unauthorized), _) | (_, Err(ApiError::Unauthorized)) => sign_out_locked(shared, &mut i),
        (Ok(me), Ok(catalog)) => {
            i.settings.user_name = Some(me.user.name);
            i.settings.company_name = me.company.as_ref().map(|c| c.name.clone());
            i.settings.company_slug = me.company.map(|c| c.slug);
            i.settings.policy = me.policy.unwrap_or_default().sanitized();
            // Drop a selection that no longer exists on the server.
            if let Some(tid) = &i.settings.task_id {
                if !catalog.tasks.iter().any(|t| &t.id == tid) {
                    i.settings.task_id = None;
                }
            }
            if let Some(pid) = &i.settings.project_id {
                if !catalog.projects.iter().any(|p| &p.id == pid) {
                    i.settings.project_id = None;
                }
            }
            i.catalog = Some(catalog);
            i.online = true;
            shared.store.save_settings(&i.settings);
        }
        _ => i.online = false,
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
        if i.tracking.is_some() {
            stop_tracking_locked(&shared, &mut i);
        }
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
fn select(app: AppHandle, state: State<'_, AppState>, project_id: Option<String>, task_id: Option<String>) {
    let mut i = state.inner.lock().unwrap();
    if i.tracking.is_some() {
        // Close the running sample so time before the switch keeps its old task.
        flush_sample(&state, &mut i, Utc::now());
    }
    i.settings.project_id = project_id.clone();
    i.settings.task_id = task_id.clone();
    if let Some(t) = i.tracking.as_mut() {
        t.acc = Accumulator::new(Utc::now(), project_id, task_id);
    }
    state.store.save_settings(&i.settings);
    emit(&app, &i);
}

#[tauri::command]
async fn create_task(app: AppHandle, state: State<'_, AppState>, title: String) -> Result<(), String> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("emptyTitle".into());
    }
    let shared = state.inner().clone();
    let (server, token, project) = {
        let i = shared.inner.lock().unwrap();
        (i.settings.server_url.clone(), i.token.clone().ok_or("signedOut")?, i.settings.project_id.clone())
    };
    let task = tauri::async_runtime::spawn_blocking(move || Api::new(&server).create_task(&token, project.as_deref(), &title))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    {
        let mut i = shared.inner.lock().unwrap();
        if let Some(c) = i.catalog.as_mut() {
            c.tasks.insert(0, task.clone());
        }
    }
    let project_id = task.project_id.clone();
    select(app, state, project_id, Some(task.id));
    Ok(())
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
    if i.tracking.is_none() {
        let now = Utc::now();
        let acc = Accumulator::new(now, i.settings.project_id.clone(), i.settings.task_id.clone());
        i.tracking = Some(Tracking::new(now, acc, &i.settings.policy.screenshots));
        i.notice = None;
    }
    emit(&app, &i);
    Ok(())
}

#[tauri::command]
fn stop_tracking(app: AppHandle, state: State<'_, AppState>) {
    let mut i = state.inner.lock().unwrap();
    if i.tracking.is_some() {
        stop_tracking_locked(&state, &mut i);
    }
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
    {
        let mut i = shared.inner.lock().unwrap();
        if i.tracking.is_some() {
            stop_tracking_locked(&shared, &mut i);
        }
    }
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
        if i.tracking.is_some() {
            stop_tracking_locked(&shared, &mut i);
        }
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

fn toggle_tracking(app: &AppHandle) {
    let state: State<'_, AppState> = app.state();
    let tracking = state.inner.lock().unwrap().tracking.is_some();
    if tracking {
        stop_tracking(app.clone(), state);
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
            let shots_queued = shots.len();
            let (tx, rx) = mpsc::channel();
            let shared: AppState = Arc::new(Shared {
                inner: Mutex::new(Inner { settings, token, queue, shots_queued, ..Default::default() }),
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
                        {
                            let mut i = state.inner.lock().unwrap();
                            if i.tracking.is_some() {
                                stop_tracking_locked(&state, &mut i);
                            }
                        }
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
            // Account, catalog and policy: now and every 15 minutes.
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
            select,
            create_task,
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
        .run(|app, event| {
            if let tauri::RunEvent::Reopen { .. } = event {
                show_main(app);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rejected_sample_index() {
        assert_eq!(rejected_index("invalid_samples samples[3]: bad id"), Some(3));
        assert_eq!(rejected_index("invalid_samples samples must be an array"), None);
        assert_eq!(rejected_index("samples[x]"), None);
    }

    #[test]
    fn clock_format() {
        assert_eq!(hms(3725), "01:02:05");
    }

    fn tracking(policy: &ScreenshotPolicy) -> Tracking {
        let now = Utc::now();
        Tracking::new(now, Accumulator::new(now, None, None), policy)
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
