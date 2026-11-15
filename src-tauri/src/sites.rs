//! Websites, by domain only. Off unless the company turns it on. When it is on,
//! a background thread asks the browser in front for the address of its active
//! tab (macOS: AppleScript, which needs the Automation permission once per
//! browser) and keeps only the host name: "https://www.github.com/acme/x?y" is
//! stored as "github.com". Paths, queries, page titles and page contents are
//! never read or kept.
//!
//! The lookup runs on its own thread so a slow browser never delays the
//! one-second activity ticks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use crate::os;

/// How often the browser is asked while website tracking is on.
const POLL: Duration = Duration::from_secs(2);

/// The host of an http(s) URL without "www.", lowercased; None for anything
/// else (file:, about:, chrome://, IP addresses, localhost, invalid input).
pub fn domain_of(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("HTTPS://"))
        .or_else(|| url.strip_prefix("HTTP://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    // Drop credentials and the port.
    let host = authority.rsplit('@').next()?;
    let host = if host.starts_with('[') { return None } else { host.split(':').next()? };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    let labels: Vec<&str> = host.split('.').collect();
    let valid_label = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    let tld = labels.last()?;
    if labels.len() < 2 || host.len() > 253 || !labels.iter().all(|l| valid_label(l)) || !tld.chars().next()?.is_ascii_alphabetic() {
        return None;
    }
    Some(host)
}

struct Watcher {
    enabled: AtomicBool,
    /// (app in front when asked, its tab's domain).
    current: Mutex<Option<(String, String)>>,
}

static WATCHER: OnceLock<Arc<Watcher>> = OnceLock::new();

fn watcher() -> &'static Arc<Watcher> {
    WATCHER.get_or_init(|| {
        let w = Arc::new(Watcher { enabled: AtomicBool::new(false), current: Mutex::new(None) });
        let bg = w.clone();
        thread::Builder::new()
            .name("sites".into())
            .spawn(move || loop {
                thread::sleep(POLL);
                let next = if bg.enabled.load(Ordering::Relaxed) {
                    os::active_browser_url().and_then(|(app, url)| domain_of(&url).map(|d| (app, d)))
                } else {
                    None
                };
                *bg.current.lock().unwrap() = next;
            })
            .expect("spawn sites thread");
        w
    })
}

/// The domain of the browser tab in front, if website tracking is `enabled`
/// and the browser that reported it is still the app in front.
pub fn current(enabled: bool, front_app: Option<&str>) -> Option<String> {
    let w = watcher();
    w.enabled.store(enabled, Ordering::Relaxed);
    if !enabled {
        return None;
    }
    let cur = w.current.lock().unwrap();
    match (cur.as_ref(), front_app) {
        (Some((app, domain)), Some(front)) if app == front => Some(domain.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::domain_of;

    #[test]
    fn keeps_only_the_host() {
        assert_eq!(domain_of("https://www.GitHub.com/acme/secret?q=1#x").as_deref(), Some("github.com"));
        assert_eq!(domain_of("http://docs.sentroy.com:8080/a").as_deref(), Some("docs.sentroy.com"));
        assert_eq!(domain_of("https://user:pw@mail.google.com/").as_deref(), Some("mail.google.com"));
        assert_eq!(domain_of("https://figma.com").as_deref(), Some("figma.com"));
    }

    #[test]
    fn ignores_everything_else() {
        for url in [
            "",
            "about:blank",
            "chrome://settings",
            "file:///Users/me/doc.pdf",
            "https://localhost:3000/",
            "http://192.168.1.10/admin",
            "https://[::1]/",
            "ftp://example.com",
            "https://bad_host.com",
            "javascript:alert(1)",
        ] {
            assert_eq!(domain_of(url), None, "{url}");
        }
    }
}
