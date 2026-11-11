//! Screenshots, only when the company enabled them in the policy and only while
//! the timer runs. Each capture is downscaled, blurred on this machine when the
//! policy asks for it (the unblurred image never leaves memory), encoded as
//! JPEG and put into an on-disk queue that the uploader drains.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use image::imageops::{self, FilterType};
use image::{DynamicImage, RgbaImage};

/// Captures are downscaled to at most this width.
pub const MAX_WIDTH: u32 = 1600;
pub const JPEG_QUALITY: u8 = 70;
/// The server accepts at most 2 MB per screenshot.
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
/// Bound on the on-disk queue; the oldest files are dropped beyond it.
pub const MAX_QUEUED: usize = 200;
/// Length of a screenshot window in tracked seconds.
pub const WINDOW_SECS: u64 = 600;

#[derive(Debug, Clone, PartialEq)]
pub struct Shot {
    pub id: String,
    pub taken_at: DateTime<Utc>,
    pub width: u32,
    pub height: u32,
    pub path: PathBuf,
}

/// Downscale, optionally blur, encode. Returns (jpeg bytes, width, height).
pub fn process(img: RgbaImage, blur: bool) -> Result<(Vec<u8>, u32, u32), String> {
    let mut img = DynamicImage::ImageRgba8(img).to_rgb8();
    if img.width() > MAX_WIDTH {
        let h = ((img.height() as u64 * MAX_WIDTH as u64) / img.width() as u64).max(1) as u32;
        img = imageops::resize(&img, MAX_WIDTH, h, FilterType::Triangle);
    }
    let (w, h) = img.dimensions();
    if blur {
        // Shrink to 1/24, blur, and scale back up: text becomes unreadable and
        // the detail cannot be recovered from the result.
        let sw = (w / 24).max(1);
        let sh = (h / 24).max(1);
        let small = imageops::resize(&img, sw, sh, FilterType::Triangle);
        let small = imageops::blur(&small, 1.5);
        img = imageops::resize(&small, w, h, FilterType::Triangle);
    }
    let mut quality = JPEG_QUALITY;
    loop {
        let mut out = Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
            .encode_image(&img)
            .map_err(|e| e.to_string())?;
        let bytes = out.into_inner();
        if bytes.len() <= MAX_BYTES || quality <= 30 {
            return Ok((bytes, w, h));
        }
        quality -= 15;
    }
}

/// `<id>_<takenAtMillis>_<width>x<height>.jpg`
fn file_name(id: &str, taken_at: DateTime<Utc>, w: u32, h: u32) -> String {
    format!("{id}_{}_{w}x{h}.jpg", taken_at.timestamp_millis())
}

fn parse(path: &Path) -> Option<Shot> {
    let stem = path.file_name()?.to_str()?.strip_suffix(".jpg")?;
    let mut parts = stem.split('_');
    let id = parts.next()?.to_string();
    let millis: i64 = parts.next()?.parse().ok()?;
    let (w, h) = parts.next()?.split_once('x')?;
    if parts.next().is_some() || id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    Some(Shot {
        id,
        taken_at: Utc.timestamp_millis_opt(millis).single()?,
        width: w.parse().ok()?,
        height: h.parse().ok()?,
        path: path.to_path_buf(),
    })
}

pub struct ShotQueue {
    dir: PathBuf,
}

impl ShotQueue {
    pub fn new(dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&dir);
        ShotQueue { dir }
    }

    /// Queued screenshots, oldest first.
    pub fn list(&self) -> Vec<Shot> {
        let mut shots: Vec<Shot> = fs::read_dir(&self.dir)
            .map(|rd| rd.filter_map(|e| parse(&e.ok()?.path())).collect())
            .unwrap_or_default();
        shots.sort_by_key(|s| s.taken_at);
        shots
    }

    pub fn len(&self) -> usize {
        self.list().len()
    }

    pub fn save(&self, jpeg: &[u8], taken_at: DateTime<Utc>, w: u32, h: u32) -> std::io::Result<Shot> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let path = self.dir.join(file_name(&id, taken_at, w, h));
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, jpeg)?;
        fs::rename(&tmp, &path)?;
        let shots = self.list();
        if shots.len() > MAX_QUEUED {
            for old in &shots[..shots.len() - MAX_QUEUED] {
                let _ = fs::remove_file(&old.path);
            }
        }
        Ok(Shot { id, taken_at, width: w, height: h, path })
    }

    pub fn remove(&self, shot: &Shot) {
        let _ = fs::remove_file(&shot.path);
    }
}

/// `n` distinct random offsets in seconds inside one window, sorted, kept away
/// from the window's edges.
pub fn plan_offsets(n: u32, seed: u128) -> Vec<u64> {
    let n = n.min(3) as usize;
    let mut out: Vec<u64> = Vec::with_capacity(n);
    let mut x = seed | 1;
    // Split the window into n equal parts and pick one moment in each, so the
    // shots are random but never bunched together.
    let part = (WINDOW_SECS - 20) / n.max(1) as u64;
    for k in 0..n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.push(10 + k as u64 * part + (x % part as u128) as u64);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noisy(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| image::Rgba([((x * 7 + y * 13) % 256) as u8, ((x ^ y) % 256) as u8, (y % 256) as u8, 255]))
    }

    #[test]
    fn downscales_wide_images() {
        let (bytes, w, h) = process(noisy(3200, 2000), false).unwrap();
        assert_eq!((w, h), (1600, 1000));
        assert_eq!(&bytes[..2], &[0xFF, 0xD8]); // JPEG SOI
        let (_, w, h) = process(noisy(800, 600), false).unwrap();
        assert_eq!((w, h), (800, 600));
    }

    #[test]
    fn blur_removes_detail() {
        let (sharp, ..) = process(noisy(800, 600), false).unwrap();
        let (blurred, ..) = process(noisy(800, 600), true).unwrap();
        // Blurred images compress far better: the high-frequency detail is gone.
        assert!(blurred.len() * 4 < sharp.len(), "{} vs {}", blurred.len(), sharp.len());
        let img = image::load_from_memory(&blurred).unwrap().to_rgb8();
        let (a, b) = (img.get_pixel(100, 100), img.get_pixel(101, 100));
        let diff: i32 = (0..3).map(|i| (a[i] as i32 - b[i] as i32).abs()).sum();
        assert!(diff < 12, "neighbouring pixels differ by {diff}");
    }

    #[test]
    fn queue_round_trips_and_is_bounded() {
        let dir = std::env::temp_dir().join(format!("sm-shots-{}", uuid::Uuid::new_v4()));
        let q = ShotQueue::new(dir.clone());
        let t = Utc.with_ymd_and_hms(2026, 11, 11, 9, 0, 0).unwrap();
        let s = q.save(b"jpeg", t, 1600, 900).unwrap();
        assert_eq!(q.list(), vec![s.clone()]);
        assert_eq!((s.width, s.height, s.taken_at), (1600, 900, t));
        for i in 0..(MAX_QUEUED as i64 + 2) {
            q.save(b"x", t + chrono::Duration::seconds(i + 1), 1, 1).unwrap();
        }
        let list = q.list();
        assert_eq!(list.len(), MAX_QUEUED);
        assert!(!list.contains(&s), "oldest dropped");
        q.remove(&list[0]);
        assert_eq!(q.len(), MAX_QUEUED - 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn ignores_foreign_files() {
        assert!(parse(Path::new("/x/notes.txt")).is_none());
        assert!(parse(Path::new("/x/a_b_c.jpg")).is_none());
        assert!(parse(Path::new("/x/ab_1700000000000_10x10.jpg")).is_some());
    }

    #[test]
    fn offsets_are_spread_inside_the_window() {
        for seed in [1u128, 42, u128::MAX, 0xdead_beef] {
            for n in 1..=3 {
                let o = plan_offsets(n, seed);
                assert_eq!(o.len(), n as usize);
                assert!(o.windows(2).all(|w| w[0] < w[1]));
                assert!(o.iter().all(|&s| (10..WINDOW_SECS - 10).contains(&s)), "{o:?}");
            }
        }
        assert!(plan_offsets(0, 7).is_empty());
    }
}
