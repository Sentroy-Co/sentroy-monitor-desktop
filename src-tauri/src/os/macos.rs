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

/// AppleScript that returns the active tab's URL, for browsers that support it
/// (Firefox has no AppleScript for this).
fn url_script(bundle_id: &str) -> Option<&'static str> {
    match bundle_id {
        "com.apple.Safari" | "com.apple.SafariTechnologyPreview" | "com.kagi.kagimacOS" => Some("URL of front document"),
        "com.google.Chrome"
        | "com.google.Chrome.beta"
        | "com.google.Chrome.canary"
        | "com.microsoft.edgemac"
        | "com.brave.Browser"
        | "company.thebrowser.Browser"
        | "com.vivaldi.Vivaldi"
        | "com.operasoftware.Opera"
        | "org.chromium.Chromium" => Some("URL of active tab of front window"),
        _ => None,
    }
}

/// The app in front and its active tab's URL when that app is a known browser.
/// Asks the browser through Apple Events (one Automation prompt per browser).
pub fn active_browser_url() -> Option<(String, String)> {
    use objc2_app_kit::NSWorkspace;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[allow(unused_unsafe)]
    let (name, bundle) = unsafe {
        let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
        (app.localizedName()?.to_string(), app.bundleIdentifier()?.to_string())
    };
    let property = url_script(&bundle)?;
    // `application id` targets the running instance; it is in front, so nothing launches.
    let script = format!("tell application id \"{bundle}\" to get {property}");
    let mut child = Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_millis(1500);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None, // denied or no window
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    let url = out.trim().to_string();
    (!url.is_empty()).then_some((name, url))
}
