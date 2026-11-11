//! Windows: GetLastInputInfo for input idle time, the foreground window's
//! process image name for the frontmost app. No permission is needed.

use windows::core::PWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

pub fn seconds_since_input() -> Option<f64> {
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    unsafe {
        if !GetLastInputInfo(&mut info).as_bool() {
            return None;
        }
        // Both are milliseconds since boot (wrapping every ~49.7 days).
        Some(GetTickCount().wrapping_sub(info.dwTime) as f64 / 1000.0)
    }
}

/// The executable name of the foreground window's process, e.g. "chrome".
pub fn frontmost_app() -> Option<String> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        ok.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let stem = std::path::Path::new(&path).file_stem()?.to_string_lossy().to_string();
        (!stem.is_empty()).then_some(stem)
    }
}

pub fn screen_capture_granted() -> bool {
    true
}

pub fn request_screen_capture() -> bool {
    true
}

pub fn screen_capture_supported() -> bool {
    true
}

pub fn capture_fallback() -> Result<image::RgbaImage, String> {
    Err("screen capture failed".into())
}

pub fn computer_name() -> Option<String> {
    std::env::var("COMPUTERNAME").ok().filter(|s| !s.trim().is_empty())
}
