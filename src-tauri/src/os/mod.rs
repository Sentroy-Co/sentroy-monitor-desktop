//! Everything the agent asks the operating system, behind one small API with a
//! per-OS implementation:
//!
//! * `seconds_since_input()` — seconds since the last keyboard/mouse event,
//!   `None` when the platform cannot tell (e.g. a pure Wayland session);
//! * `frontmost_app()` — the name of the app in front (never a window title);
//! * `active_browser_url()` — the active tab's URL when the app in front is a
//!   supported browser (macOS only; used for the domain, see `sites`);
//! * `screen_capture_granted()` / `request_screen_capture()` — the macOS Screen
//!   Recording permission (always granted elsewhere);
//! * `capture_displays()` — one screenshot of all displays (composed by
//!   `screenshots::compose`).

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as imp;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use self::windows as imp;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as imp;

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
mod imp {
    pub fn seconds_since_input() -> Option<f64> {
        None
    }
    pub fn frontmost_app() -> Option<String> {
        None
    }
    pub fn screen_capture_granted() -> bool {
        false
    }
    pub fn request_screen_capture() -> bool {
        false
    }
    pub fn capture_fallback() -> Result<image::RgbaImage, String> {
        Err("unsupported platform".into())
    }
    pub fn screen_capture_supported() -> bool {
        false
    }
    pub fn computer_name() -> Option<String> {
        None
    }
    pub fn active_browser_url() -> Option<(String, String)> {
        None
    }
}

pub use imp::{active_browser_url, computer_name, frontmost_app, request_screen_capture, screen_capture_granted, screen_capture_supported, seconds_since_input};

/// Where the user grants a permission in the system settings, if anywhere.
pub fn permission_settings_url(kind: &str) -> Option<&'static str> {
    match (kind, std::env::consts::OS) {
        ("screen", "macos") => Some("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"),
        ("notifications", "macos") => Some("x-apple.systempreferences:com.apple.preference.notifications"),
        ("notifications", "windows") => Some("ms-settings:notifications"),
        _ => None,
    }
}

/// One screenshot of every display as RGBA: the primary display first, then
/// the others left to right (top to bottom on ties), composed into one image
/// by `screenshots::compose`. Falls back to the platform's single-display
/// capture when xcap cannot capture any display.
pub fn capture_displays() -> Result<image::RgbaImage, String> {
    if !screen_capture_supported() {
        return Err("screen capture is not supported in this session".into());
    }
    let xcap = (|| {
        let mut monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
        monitors.sort_by_key(|m| (!m.is_primary().unwrap_or(false), m.x().unwrap_or(0), m.y().unwrap_or(0)));
        let mut last_err = String::from("no display");
        let mut images = Vec::with_capacity(monitors.len());
        for m in &monitors {
            match m.capture_image() {
                Ok(img) if img.width() > 0 && img.height() > 0 => images.push(img),
                Ok(_) => last_err = "empty capture".into(),
                Err(e) => last_err = e.to_string(),
            }
        }
        if images.is_empty() {
            return Err(last_err);
        }
        Ok(crate::screenshots::compose(images))
    })();
    match xcap {
        Ok(img) => Ok(img),
        Err(e) => imp::capture_fallback().map_err(|f| format!("{e}; fallback: {f}")),
    }
}

#[cfg(test)]
mod tests {
    /// Needs a display (and Screen Recording on macOS): `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn captures_all_displays() {
        let img = super::capture_displays().expect("capture");
        assert!(img.width() > 100 && img.height() > 100);
        let (jpeg, w, _) = crate::screenshots::process(img, true).unwrap();
        assert!(w <= crate::screenshots::MAX_WIDTH && jpeg.len() < crate::screenshots::MAX_BYTES);
    }
}
