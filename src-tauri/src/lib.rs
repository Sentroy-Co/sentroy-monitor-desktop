//! Sentroy Monitor desktop agent.
//!
//! One background thread samples activity once a second while the timer runs
//! and cuts one-minute samples into an offline queue; another uploads the
//! queue. The UI (ui/) talks to this core through the commands below and
//! re-renders on the `state` event.

mod activity;
mod api;
mod store;

use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_opener::OpenerExt;

use activity::{activity_percent, Accumulator, Sample, SAMPLE_SECS};
use api::{Api, ApiError, Catalog, DeviceStart};
use store::{push_bounded, Settings, Store};

/// Stop the timer after this much continuous inactivity.
const AUTO_STOP_IDLE_SECS: f64 = 30.0 * 60.0;
const UPLOAD_EVERY: Duration = Duration::from_secs(30);
const UPLOAD_BATCH: usize = 200;

struct Tracking {
    acc: Accumulator,
    started_at: DateTime<Utc>,
    session_seconds: u64,
    /// Recent per-second activity for the live percentage (last 10 minutes).
    recent: std::collections::VecDeque<(bool, bool)>,
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
}

struct Shared {
    inner: Mutex<Inner>,
    store: Store,
    nudge: Mutex<mpsc::Sender<()>>,
}

type AppState = Arc<Shared>;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    version: &'static str,
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
    activity_percent: Option<u32>,
    queued: usize,
    online: bool,
    last_sync: Option<DateTime<Utc>>,
    notice: Option<String>,
}

fn local_day() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

fn snapshot(i: &Inner) -> Snapshot {
    let today_seconds = if i.settings.today.as_deref() == Some(&local_day()) { i.settings.today_seconds } else { 0 };
    Snapshot {
        version: env!("CARGO_PKG_VERSION"),
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
        activity_percent: i.tracking.as_ref().map(|t| {
            let tracked = t.recent.len() as u32;
            let active = t.recent.iter().filter(|(a, _)| *a).count() as u32;
            let idle = t.recent.iter().filter(|(_, i)| *i).count() as u32;
            activity_percent(active, tracked, idle)
        }),
        queued: i.queue.len(),
        online: i.online,
        last_sync: i.last_sync,
        notice: i.notice.clone(),
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

/// Closes the current sample (if any) into the queue.
fn flush_sample(shared: &Shared, i: &mut Inner, now: DateTime<Utc>) {
    if let Some(t) = i.tracking.as_mut() {
        let project = i.settings.project_id.clone();
        let task = i.settings.task_id.clone();
        let acc = std::mem::replace(&mut t.acc, Accumulator::new(now, project, task));
        if let Some(s) = acc.finish(now) {
            push_bounded(&mut i.queue, s);
            shared.store.save_queue(&i.queue);
        }
    }
}

fn stop_tracking_locked(shared: &Shared, i: &mut Inner) {
    flush_sample(shared, i, Utc::now());
    i.tracking = None;
    shared.store.save_settings(&i.settings);
    let _ = shared.nudge.lock().map(|n| n.send(()));
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
    shared.store.save_settings(&i.settings);
}

fn ticker(app: AppHandle, shared: AppState) {
    loop {
        thread::sleep(Duration::from_secs(1));
        // Ask the OS outside the lock.
        let since_input = activity::seconds_since_input();
        let front = activity::frontmost_app();
        let mut i = shared.inner.lock().unwrap();
        let now = Utc::now();
        if let Some(t) = i.tracking.as_mut() {
            t.acc.tick(since_input, front.as_deref());
            t.session_seconds += 1;
            t.recent.push_back((since_input < activity::ACTIVE_WINDOW_SECS, since_input >= activity::IDLE_THRESHOLD_SECS));
            while t.recent.len() > 600 {
                t.recent.pop_front();
            }
            let cut = (now - t.started_at).num_seconds() > 0 && t.acc.tracked_seconds() as i64 >= SAMPLE_SECS;
            let day = local_day();
            if i.settings.today.as_deref() != Some(&day) {
                i.settings.today = Some(day);
                i.settings.today_seconds = 0;
            }
            i.settings.today_seconds += 1;
            if cut {
                flush_sample(&shared, &mut i, now);
                shared.store.save_settings(&i.settings);
            }
            if since_input >= AUTO_STOP_IDLE_SECS {
                stop_tracking_locked(&shared, &mut i);
                i.notice = Some("autoStopped".into());
            }
        }
        emit(&app, &i);
    }
}

fn uploader(app: AppHandle, shared: AppState, rx: mpsc::Receiver<()>) {
    loop {
        let _ = rx.recv_timeout(UPLOAD_EVERY);
        let (server, token, batch) = {
            let i = shared.inner.lock().unwrap();
            match &i.token {
                Some(t) if !i.queue.is_empty() => {
                    (i.settings.server_url.clone(), t.clone(), i.queue.iter().take(UPLOAD_BATCH).cloned().collect::<Vec<_>>())
                }
                _ => continue,
            }
        };
        let result = Api::new(&server).upload(&token, &batch);
        let mut i = shared.inner.lock().unwrap();
        match result {
            Ok(()) => {
                let sent: std::collections::HashSet<&str> = batch.iter().map(|s| s.id.as_str()).collect();
                i.queue.retain(|s| !sent.contains(s.id.as_str()));
                shared.store.save_queue(&i.queue);
                i.online = true;
                i.last_sync = Some(Utc::now());
                if !i.queue.is_empty() {
                    let _ = shared.nudge.lock().map(|n| n.send(()));
                }
            }
            Err(ApiError::Unauthorized) => {
                sign_out_locked(&shared, &mut i);
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
                let _ = shared.nudge.lock().map(|n| n.send(()));
            }
            Err(_) => i.online = false,
        }
        emit(&app, &i);
    }
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
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("/usr/sbin/scutil").args(["--get", "ComputerName"]).output() {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !name.is_empty() {
            return name.chars().take(80).collect();
        }
    }
    "Desktop".into()
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
                    if let Some(w) = app2.get_webview_window("main") {
                        let _ = w.show();
                        let _ = w.set_focus();
                    }
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
    // Try to deliver queued time before the token goes away.
    if let Some(token) = token.clone() {
        let s2 = shared.clone();
        let srv = server.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || {
            let batch: Vec<Sample> = s2.inner.lock().unwrap().queue.clone();
            let api = Api::new(&srv);
            for chunk in batch.chunks(UPLOAD_BATCH) {
                if api.upload(&token, chunk).is_err() {
                    break;
                }
                let ids: std::collections::HashSet<String> = chunk.iter().map(|s| s.id.clone()).collect();
                let mut i = s2.inner.lock().unwrap();
                i.queue.retain(|s| !ids.contains(&s.id));
                s2.store.save_queue(&i.queue);
            }
            let _ = api.logout(&token);
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
    if i.tracking.is_none() {
        let now = Utc::now();
        let acc = Accumulator::new(now, i.settings.project_id.clone(), i.settings.task_id.clone());
        i.tracking = Some(Tracking { acc, started_at: now, session_seconds: 0, recent: Default::default() });
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
        .setup(|app| {
            let dir = app.path().app_data_dir().expect("app data dir");
            let store = Store::new(dir);
            let settings = store.load_settings();
            let token = Store::load_token(&settings.server_url);
            let queue = store.load_queue();
            let (tx, rx) = mpsc::channel();
            let shared: AppState = Arc::new(Shared {
                inner: Mutex::new(Inner { settings, token, queue, ..Default::default() }),
                store,
                nudge: Mutex::new(tx),
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
            let h = app.handle().clone();
            let s = shared.clone();
            thread::spawn(move || refresh_account(&h, &s));
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps the agent (and the timer) running in the menu bar.
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
}
