//! Linux (X11): MIT-SCREEN-SAVER for input idle time, `_NET_ACTIVE_WINDOW` +
//! `_NET_WM_PID` for the frontmost app's process name. Pure Rust (x11rb), no
//! window titles are read.
//!
//! On Wayland an X client cannot see input or windows of native Wayland apps,
//! so the agent reports both as unknown (time is still tracked, activity is
//! not measured and the idle auto-stop does not fire) and does not take
//! screenshots (the portal would prompt the user every time).

use std::sync::OnceLock;

use x11rb::connection::Connection;
use x11rb::protocol::screensaver::ConnectionExt as _;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, Window};
use x11rb::rust_connection::RustConnection;

struct X {
    conn: RustConnection,
    root: Window,
    active_window: Atom,
    wm_pid: Atom,
}

fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").map(|s| s.eq_ignore_ascii_case("wayland")).unwrap_or(false)
}

fn x() -> Option<&'static X> {
    static X11: OnceLock<Option<X>> = OnceLock::new();
    X11.get_or_init(|| {
        if is_wayland() {
            return None;
        }
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let atom = |name: &[u8]| conn.intern_atom(false, name).ok()?.reply().ok().map(|r| r.atom);
        let active_window = atom(b"_NET_ACTIVE_WINDOW")?;
        let wm_pid = atom(b"_NET_WM_PID")?;
        Some(X { conn, root, active_window, wm_pid })
    })
    .as_ref()
}

pub fn seconds_since_input() -> Option<f64> {
    let x = x()?;
    let info = x.conn.screensaver_query_info(x.root).ok()?.reply().ok()?;
    Some(info.ms_since_user_input as f64 / 1000.0)
}

pub fn frontmost_app() -> Option<String> {
    let x = x()?;
    let win = x
        .conn
        .get_property(false, x.root, x.active_window, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?
        .value32()?
        .next()?;
    if win == 0 {
        return None;
    }
    let pid = x.conn.get_property(false, win, x.wm_pid, AtomEnum::CARDINAL, 0, 1).ok()?.reply().ok()?.value32()?.next()?;
    let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));
    let name = exe.or_else(|| std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string()))?;
    (!name.is_empty()).then_some(name)
}

pub fn screen_capture_granted() -> bool {
    true
}

pub fn request_screen_capture() -> bool {
    true
}

pub fn screen_capture_supported() -> bool {
    !is_wayland()
}

pub fn capture_fallback() -> Result<image::RgbaImage, String> {
    Err("screen capture failed".into())
}

pub fn computer_name() -> Option<String> {
    std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
