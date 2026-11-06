//! Image thumbnails: decoded by a few worker threads, cached on disk as JPEG.
//!
//! JPEG XL goes through `jxl-oxide`; everything else through the `image` crate.
//! Results come back on an async channel the UI polls from the GLib main loop.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Longest side of a thumbnail, in pixels.
pub const THUMB_SIZE: u32 = 320;

pub struct Thumbnails {
    jobs: async_channel::Sender<PathBuf>,
    cache_dir: Arc<PathBuf>,
    tx: async_channel::Sender<(PathBuf, Result<PathBuf, String>)>,
    pub results: async_channel::Receiver<(PathBuf, Result<PathBuf, String>)>,
    /// Images queued or being decoded right now, so repeated requests don't decode twice.
    in_flight: Arc<std::sync::Mutex<std::collections::HashSet<PathBuf>>>,
}

impl Thumbnails {
    pub fn start() -> Self {
        let cache_dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("realityscan-gtk/thumbs");
        let _ = std::fs::create_dir_all(&cache_dir);
        // Each JXL decode is itself multi-threaded on rayon's global pool, so a few decodes at a
        // time keep every core busy without thrashing. The workers are plain threads on purpose:
        // inside a rayon pool, jxl-oxide's fork-join work would stay confined to that pool's few
        // threads and a 45-megapixel decode takes ten times longer.
        let jobs = (std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) / 6).clamp(2, 4);
        let (jobs_tx, jobs_rx) = async_channel::unbounded::<PathBuf>();
        let (tx, results) = async_channel::unbounded();
        let cache_dir = Arc::new(cache_dir);
        let in_flight = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        for i in 0..jobs {
            let rx = jobs_rx.clone();
            let tx = tx.clone();
            let cache_dir = Arc::clone(&cache_dir);
            let in_flight = Arc::clone(&in_flight);
            std::thread::Builder::new()
                .name(format!("thumb-{i}"))
                .spawn(move || {
                    // Decoding competes with RealityScan's own import of the same images;
                    // let it have the cores first.
                    // SAFETY: setpriority on the calling thread (who = 0) only changes niceness.
                    unsafe {
                        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
                    }
                    while let Ok(path) = rx.recv_blocking() {
                        let started = std::time::Instant::now();
                        let result = cache_path(&cache_dir, &path)
                            .ok_or_else(|| "file not found".to_owned())
                            .and_then(|out| make_thumbnail(&path, &out).map(|()| out));
                        tracing::debug!(image = %path.display(), elapsed = ?started.elapsed(), "thumbnail");
                        if let Ok(mut set) = in_flight.lock() {
                            set.remove(&path);
                        }
                        let _ = tx.send_blocking((path, result));
                    }
                })
                .expect("thumbnail worker");
        }
        Self { jobs: jobs_tx, cache_dir, tx, results, in_flight }
    }

    /// Images queued or currently being decoded.
    pub fn pending(&self) -> usize {
        self.in_flight.lock().map(|s| s.len()).unwrap_or(0)
    }

    /// Request a thumbnail; the answer arrives on [`Thumbnails::results`].
    /// Cached thumbnails are answered immediately.
    pub fn request(&self, path: PathBuf) {
        let cached = cache_path(&self.cache_dir, &path);
        if let Some(cached) = &cached
            && cached.is_file()
        {
            let _ = self.tx.send_blocking((path, Ok(cached.clone())));
            return;
        }
        if let Ok(mut set) = self.in_flight.lock()
            && !set.insert(path.clone())
        {
            return; // already queued
        }
        let _ = self.jobs.send_blocking(path);
    }
}

/// Cache file for an image, keyed on path, size and modification time.
fn cache_path(cache_dir: &Path, image: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(image).ok()?;
    let mut h = DefaultHasher::new();
    image.hash(&mut h);
    meta.len().hash(&mut h);
    meta.modified().ok()?.hash(&mut h);
    Some(cache_dir.join(format!("{:016x}.jpg", h.finish())))
}

fn make_thumbnail(image: &Path, out: &Path) -> Result<(), String> {
    let decoded = decode(image).map_err(|e| e.to_string())?;
    let thumb = decoded.thumbnail(THUMB_SIZE, THUMB_SIZE).to_rgb8();
    let tmp = out.with_extension("jpg.part");
    let mut file = std::io::BufWriter::new(std::fs::File::create(&tmp).map_err(|e| e.to_string())?);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, 85)
        .encode_image(&thumb)
        .map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&tmp, out).map_err(|e| e.to_string())
}

/// Decode an image file (JPEG XL through `jxl-oxide`, everything else through `image`).
pub fn decode(path: &Path) -> Result<image::DynamicImage, Box<dyn std::error::Error + Send + Sync>> {
    let is_jxl = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("jxl"));
    if is_jxl {
        let file = std::io::BufReader::new(std::fs::File::open(path)?);
        let decoder = jxl_oxide::integration::JxlDecoder::new(file)?;
        Ok(image::DynamicImage::from_decoder(decoder)?)
    } else {
        Ok(image::ImageReader::open(path)?.with_guessed_format()?.decode()?)
    }
}
