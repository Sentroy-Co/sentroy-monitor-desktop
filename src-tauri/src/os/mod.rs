//! Everything the agent asks the operating system, behind one small API with a
//! per-OS implementation:
//!
//! * `seconds_since_input()` — seconds since the last keyboard/mouse event,
//!   `None` when the platform cannot tell (e.g. a pure Wayland session);
//! * `frontmost_app()` — the name of the app in front (never a window title);
//! * `screen_capture_granted()` / `request_screen_capture()` — the macOS Screen
//!   Recording permission (always granted elsewhere);
//! * `capture_main_display()` — a screenshot of the primary display.

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
}

pub use imp::{computer_name, frontmost_app, request_screen_capture, screen_capture_granted, screen_capture_supported, seconds_since_input};

/// Where the user grants a permission in the system settings, if anywhere.
pub fn permission_settings_url(kind: &str) -> Option<&'static str> {
    match (kind, std::env::consts::OS) {
        ("screen", "macos") => Some("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"),
        ("notifications", "macos") => Some("x-apple.systempreferences:com.apple.preference.notifications"),
        ("notifications", "windows") => Some("ms-settings:notifications"),
        _ => None,
    }
}

/// Screenshot of the primary display as RGBA.
pub fn capture_main_display() -> Result<image::RgbaImage, String> {
    if !screen_capture_supported() {
        return Err("screen capture is not supported in this session".into());
    }
    let xcap = (|| {
        let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
        let main = monitors
            .iter()
            .find(|m| m.is_primary().unwrap_or(false))
            .or_else(|| monitors.first())
            .ok_or("no display")?;
        main.capture_image().map_err(|e| e.to_string())
    })();
    match xcap {
        Ok(img) if img.width() > 0 && img.height() > 0 => Ok(img),
        Ok(_) => imp::capture_fallback(),
        Err(e) => imp::capture_fallback().map_err(|f| format!("{e}; fallback: {f}")),
    }
}

#[cfg(test)]
mod tests {
    /// Needs a display (and Screen Recording on macOS): `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn captures_the_main_display() {
        let img = super::capture_main_display().expect("capture");
        assert!(img.width() > 100 && img.height() > 100);
        let (jpeg, w, _) = crate::screenshots::process(img, true).unwrap();
        assert!(w <= crate::screenshots::MAX_WIDTH && jpeg.len() < crate::screenshots::MAX_BYTES);
    }
}
