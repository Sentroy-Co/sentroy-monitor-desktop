//! macOS: CoreGraphics for input idle time and the Screen Recording permission,
//! NSWorkspace for the frontmost app.

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// kCGEventSourceStateCombinedSessionState = 0, kCGAnyInputEventType = ~0.
pub fn seconds_since_input() -> Option<f64> {
    Some(unsafe { CGEventSourceSecondsSinceLastEventType(0, u32::MAX) })
}

pub fn frontmost_app() -> Option<String> {
    use objc2_app_kit::NSWorkspace;
    #[allow(unused_unsafe)]
    unsafe {
        let ws = NSWorkspace::sharedWorkspace();
        let app = ws.frontmostApplication()?;
        let name = app.localizedName()?;
        Some(name.to_string())
    }
}

/// Whether this process may capture the screen. macOS only re-reads the
/// permission when the app starts, so a grant needs a restart to show up here.
pub fn screen_capture_granted() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Shows the system prompt (first time) and adds the app to the Screen
/// Recording list in System Settings.
pub fn request_screen_capture() -> bool {
    unsafe { CGRequestScreenCaptureAccess() }
}

pub fn screen_capture_supported() -> bool {
    true
}

/// `screencapture` as a fallback when CoreGraphics capture via xcap fails.
pub fn capture_fallback() -> Result<image::RgbaImage, String> {
    let path = std::env::temp_dir().join(format!("sentroy-monitor-{}.png", uuid::Uuid::new_v4().simple()));
    let status = std::process::Command::new("/usr/sbin/screencapture")
        .args(["-x", "-m", "-t", "png"])
        .arg(&path)
        .status()
        .map_err(|e| e.to_string())?;
    let result = if status.success() {
        image::open(&path).map(|i| i.to_rgba8()).map_err(|e| e.to_string())
    } else {
        Err(format!("screencapture exited with {status}"))
    };
    let _ = std::fs::remove_file(&path);
    result
}

pub fn computer_name() -> Option<String> {
    let out = std::process::Command::new("/usr/sbin/scutil").args(["--get", "ComputerName"]).output().ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}
