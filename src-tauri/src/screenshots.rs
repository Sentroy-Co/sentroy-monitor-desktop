//! Screenshots, only when the company enabled them in the policy and only while
//! the timer runs. With several displays, all of them are put into one image
//! (`compose`). Each capture is downscaled, blurred on this machine when the
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
/// Space between displays in a composed screenshot.
const GAP: u32 = 8;
/// Background of a composed screenshot (gaps, letterboxing, empty cells).
const BACKDROP: image::Rgba<u8> = image::Rgba([24, 24, 27, 255]);

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

/// Puts the captures of all displays (in display order) into one image: one
/// display as is, two side by side, three or more in a two-column grid, row by
/// row. Every display gets a cell of the same size and is fitted into it
/// keeping its aspect ratio (letterboxed on a dark backdrop). Cells are sized
/// so the composite is about `MAX_WIDTH` wide, so `process` barely rescales it.
pub fn compose(mut images: Vec<RgbaImage>) -> RgbaImage {
    images.retain(|i| i.width() > 0 && i.height() > 0);
    match images.len() {
        0 => return RgbaImage::from_pixel(1, 1, BACKDROP),
        1 => return images.pop().expect("one image"),
        _ => {}
    }
    let n = images.len() as u32;
    let cols = 2;
    let rows = n.div_ceil(cols);
    let widest = images.iter().map(|i| i.width()).max().unwrap_or(1);
    let cell_w = ((MAX_WIDTH - GAP * (cols - 1)) / cols).min(widest).max(1);
    // Tall enough for the tallest aspect ratio, so no display is shrunk more than needed.
    let ratio = images.iter().map(|i| i.height() as f64 / i.width() as f64).fold(0.0, f64::max);
    let cell_h = ((cell_w as f64 * ratio).round() as u32).max(1);
    let mut out = RgbaImage::from_pixel(cols * cell_w + GAP * (cols - 1), rows * cell_h + GAP * (rows - 1), BACKDROP);
    for (k, img) in images.iter().enumerate() {
        let (col, row) = (k as u32 % cols, k as u32 / cols);
        let scale = (cell_w as f64 / img.width() as f64).min(cell_h as f64 / img.height() as f64);
        let w = ((img.width() as f64 * scale).round() as u32).clamp(1, cell_w);
        let h = ((img.height() as f64 * scale).round() as u32).clamp(1, cell_h);
        let x = col * (cell_w + GAP) + (cell_w - w) / 2;
        let y = row * (cell_h + GAP) + (cell_h - h) / 2;
        if (w, h) == img.dimensions() {
            imageops::overlay(&mut out, img, x as i64, y as i64);
        } else {
            imageops::overlay(&mut out, &imageops::resize(img, w, h, FilterType::Triangle), x as i64, y as i64);
        }
    }
    out
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

    fn solid(w: u32, h: u32, c: [u8; 3]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, image::Rgba([c[0], c[1], c[2], 255]))
    }

    const RED: [u8; 3] = [220, 20, 20];
    const GREEN: [u8; 3] = [20, 200, 20];
    const BLUE: [u8; 3] = [20, 20, 220];

    fn rgb(img: &RgbaImage, x: u32, y: u32) -> [u8; 3] {
        let p = img.get_pixel(x, y);
        [p[0], p[1], p[2]]
    }

    #[test]
    fn compose_keeps_a_single_display_as_is() {
        let img = noisy(1920, 1080);
        assert_eq!(compose(vec![img.clone()]), img);
        assert_eq!(compose(Vec::new()).dimensions(), (1, 1));
    }

    #[test]
    fn compose_puts_two_displays_side_by_side() {
        // A wide 2560x1440 display and a portrait 1080x1920 one.
        let out = compose(vec![solid(2560, 1440, RED), solid(1080, 1920, GREEN)]);
        let cell_w = (MAX_WIDTH - GAP) / 2;
        let cell_h = (cell_w as f64 * 1920.0 / 1080.0).round() as u32;
        assert_eq!(out.dimensions(), (2 * cell_w + GAP, cell_h));
        assert!(out.width() <= MAX_WIDTH);
        // Each display sits in the middle of its own cell.
        assert_eq!(rgb(&out, cell_w / 2, cell_h / 2), RED);
        assert_eq!(rgb(&out, cell_w + GAP + cell_w / 2, cell_h / 2), GREEN);
        // The landscape display is letterboxed: above it is the backdrop.
        assert_eq!(rgb(&out, cell_w / 2, 2), [BACKDROP[0], BACKDROP[1], BACKDROP[2]]);
        // The gap between the cells too.
        assert_eq!(rgb(&out, cell_w + GAP / 2, cell_h / 2), [BACKDROP[0], BACKDROP[1], BACKDROP[2]]);
    }

    #[test]
    fn compose_puts_three_displays_in_a_two_column_grid() {
        let out = compose(vec![solid(1920, 1080, RED), solid(1280, 1024, GREEN), solid(800, 600, BLUE)]);
        let cell_w = (MAX_WIDTH - GAP) / 2;
        let cell_h = (cell_w as f64 * 1024.0 / 1280.0).round() as u32;
        assert_eq!(out.dimensions(), (2 * cell_w + GAP, 2 * cell_h + GAP));
        assert_eq!(rgb(&out, cell_w / 2, cell_h / 2), RED);
        assert_eq!(rgb(&out, cell_w + GAP + cell_w / 2, cell_h / 2), GREEN);
        assert_eq!(rgb(&out, cell_w / 2, cell_h + GAP + cell_h / 2), BLUE);
        // The fourth cell stays empty.
        assert_eq!(rgb(&out, cell_w + GAP + cell_w / 2, cell_h + GAP + cell_h / 2), [BACKDROP[0], BACKDROP[1], BACKDROP[2]]);
        // And the composite still goes through the normal size limits.
        let (jpeg, w, _) = process(out, true).unwrap();
        assert!(w <= MAX_WIDTH && jpeg.len() <= MAX_BYTES);
    }

    #[test]
    fn compose_cells_are_never_wider_than_the_widest_display() {
        let out = compose(vec![solid(400, 300, RED), solid(300, 300, GREEN)]);
        assert_eq!(out.dimensions(), (2 * 400 + GAP, 400));
        assert_eq!(rgb(&out, 200, 200), RED);
        assert_eq!(rgb(&out, 400 + GAP + 200, 200), GREEN);
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
