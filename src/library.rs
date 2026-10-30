//! The photo library: the shared folder, and a cache of each photo already fitted to the display (so the
//! slow decode of a big HEIC happens once, in the background, never at refresh time).

use crate::decode::{is_photo_name, prepare_file, Limits};
use crate::overlay::{DISPLAY_H, DISPLAY_W};
use crate::render::Prepared;
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Path below the shared folder, with `/` separators.
    pub rel: String,
    pub path: PathBuf,
    pub size: u64,
    pub mtime: i64,
    /// Names the cache file; changes when the file or the bar colour changes.
    pub key: String,
}

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
}

pub fn key_for(rel: &str, size: u64, mtime: i64, bars: [u8; 3]) -> String {
    format!("{:016x}", fnv1a(format!("{rel}\0{size}\0{mtime}\0{}", bars[0]).as_bytes()))
}

fn walk(base: &Path, dir: &Path, depth: u32, bars: [u8; 3], out: &mut Vec<Item>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() && depth < 3 && !name.starts_with('.') {
            walk(base, &e.path(), depth + 1, bars, out);
        } else if ft.is_file() && is_photo_name(&name) {
            let Ok(md) = e.metadata() else { continue };
            if md.len() == 0 {
                continue;
            }
            let mtime = md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
            let rel = e.path().strip_prefix(base).unwrap_or(&e.path()).to_string_lossy().replace('\\', "/");
            let key = key_for(&rel, md.len(), mtime, bars);
            out.push(Item { rel, path: e.path(), size: md.len(), mtime, key });
        }
    }
}

/// All photos in the shared folder (and up to three folders deep), sorted by name.
pub fn scan(dir: &Path, bars: [u8; 3]) -> Vec<Item> {
    let mut out = Vec::new();
    walk(dir, dir, 0, bars, &mut out);
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

/// What one look at the shared folder found.
#[derive(Debug, Default)]
pub struct Snapshot {
    /// Settled photos that still have to be decoded (not cached, not known to be broken).
    pub pending: Vec<Item>,
    /// Keys of settled photos whose fitted copy exists.
    pub ready: Vec<String>,
    /// Cache files that belong to no photo any more, and leftover temporary files. Delete them only while no
    /// worker is writing.
    pub stale: Vec<PathBuf>,
}

pub struct Library {
    pub photos_dir: PathBuf,
    cache_dir: PathBuf,
    bars: [u8; 3],
    limits: Limits,
    /// size and mtime seen on the previous scan: a file is settled once it has not changed between scans.
    seen: HashMap<String, (u64, i64)>,
    first_scan: bool,
    /// Shared with the worker thread, which records a photo it could not decode before it reports back.
    failed: Arc<Mutex<HashSet<String>>>,
}

impl Library {
    pub fn new(photos_dir: &Path, state_dir: &Path, bars: [u8; 3], limits: Limits) -> Self {
        Library {
            photos_dir: photos_dir.into(),
            cache_dir: state_dir.join("cache"),
            bars,
            limits,
            seen: HashMap::new(),
            first_scan: true,
            failed: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    fn cache_path(&self, key: &str) -> PathBuf {
        self.cache_dir.join(format!("{key}.jpg"))
    }

    pub fn is_ready(&self, key: &str) -> bool {
        self.cache_path(key).is_file()
    }

    /// File names in the cache folder, one listing for a whole scan.
    fn cache_listing(&self) -> Vec<(String, PathBuf)> {
        let Ok(rd) = std::fs::read_dir(&self.cache_dir) else { return Vec::new() };
        rd.flatten().map(|e| (e.file_name().to_string_lossy().into_owned(), e.path())).collect()
    }

    fn stable(&self, it: &Item, first: bool) -> bool {
        first || self.seen.get(&it.rel) == Some(&(it.size, it.mtime))
    }

    /// Look at the folder once: what needs decoding, what can be shown, what is left over. A photo counts once its
    /// size and modification time have not changed since the previous call (at startup everything counts).
    pub fn rescan(&mut self) -> Snapshot {
        let items = scan(&self.photos_dir, self.bars);
        let first = std::mem::replace(&mut self.first_scan, false);
        let listing = self.cache_listing();
        let cached: HashSet<&str> = listing.iter().filter_map(|(n, _)| n.strip_suffix(".jpg")).collect();
        let mut snap = Snapshot::default();
        let mut live = HashSet::new();
        let mut now_seen = HashMap::new();
        {
            let failed = self.failed.lock().unwrap_or_else(PoisonError::into_inner);
            for it in items {
                let stable = self.stable(&it, first);
                now_seen.insert(it.rel.clone(), (it.size, it.mtime));
                live.insert(it.key.clone());
                if !stable {
                    continue;
                }
                if cached.contains(it.key.as_str()) {
                    snap.ready.push(it.key);
                } else if !failed.contains(&it.key) {
                    snap.pending.push(it);
                }
            }
        }
        self.seen = now_seen;
        for (name, path) in listing {
            let stem = name.strip_suffix(".jpg");
            if name.ends_with(".tmp") || stem.is_some_and(|s| !s.is_empty() && !live.contains(s)) {
                snap.stale.push(path);
            }
        }
        snap
    }

    /// Keys of settled photos whose fitted copy exists. Does not move the "settled" bookmark that `rescan` keeps,
    /// so asking in between scans cannot make a half-copied file look finished.
    pub fn ready_keys(&self) -> Vec<String> {
        let first = self.first_scan;
        let cached: HashSet<String> = self.cache_listing().into_iter().filter_map(|(n, _)| n.strip_suffix(".jpg").map(String::from)).collect();
        scan(&self.photos_dir, self.bars).into_iter().filter(|i| self.stable(i, first) && cached.contains(&i.key)).map(|i| i.key).collect()
    }

    /// Photos still to be decoded (not cached, not known to be broken).
    pub fn pending(&mut self) -> Vec<Item> {
        self.rescan().pending
    }

    pub fn mark_failed(&self, key: &str) {
        self.failed.lock().unwrap_or_else(PoisonError::into_inner).insert(key.into());
    }

    /// The list a worker thread adds to when a photo cannot be decoded.
    pub fn failed_handle(&self) -> Arc<Mutex<HashSet<String>>> {
        self.failed.clone()
    }

    /// Decode one photo and store its fitted copy. Slow for big HEICs: run it on a worker thread.
    pub fn process(&self, item: &Item) -> Result<()> {
        process_item(&self.cache_dir, item, self.bars, &self.limits)
    }

    pub fn load(&self, key: &str) -> Result<Prepared> {
        let bytes = std::fs::read(self.cache_path(key)).with_context(|| format!("no cached copy for {key}"))?;
        let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)?.into_rgb8();
        anyhow::ensure!(img.width() as usize == DISPLAY_W && img.height() as usize == DISPLAY_H, "cache entry has the wrong size");
        Ok(Prepared { rgb: img.into_raw() })
    }

    /// Throw away a cached copy that cannot be read, so the next scan decodes the photo again.
    pub fn discard(&self, key: &str) {
        let _ = std::fs::remove_file(self.cache_path(key));
    }

    /// Delete the files `rescan` reported as stale.
    pub fn remove_stale(&self, stale: &[PathBuf]) {
        for p in stale {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Free function so a worker thread can run it without borrowing the library.
pub fn process_item(cache_dir: &Path, item: &Item, bars: [u8; 3], limits: &Limits) -> Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    let prepared = prepare_file(&item.path, bars, limits)?;
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 92)
        .encode(&prepared.rgb, DISPLAY_W as u32, DISPLAY_H as u32, image::ExtendedColorType::Rgb8)?;
    crate::fsutil::write_atomic(&cache_dir.join(format!("{}.jpg", item.key)), &jpeg)?;
    Ok(())
}

impl Library {
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }
    pub fn bars(&self) -> [u8; 3] {
        self.bars
    }
    pub fn limits(&self) -> Limits {
        self.limits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(path: &Path, w: u32, h: u32) {
        image::RgbImage::from_pixel(w, h, image::Rgb([200, 30, 30])).save(path).unwrap();
    }

    #[test]
    fn scan_finds_photos_only() {
        let d = tempfile::tempdir().unwrap();
        png(&d.path().join("a.png"), 10, 10);
        std::fs::write(d.path().join("IMG_1.MOV"), b"x").unwrap();
        std::fs::write(d.path().join(".hidden.jpg"), b"x").unwrap();
        std::fs::write(d.path().join("empty.jpg"), b"").unwrap();
        std::fs::create_dir(d.path().join("Trip")).unwrap();
        png(&d.path().join("Trip/b.png"), 10, 10);
        let items = scan(d.path(), [255; 3]);
        let names: Vec<_> = items.iter().map(|i| i.rel.as_str()).collect();
        assert_eq!(names, ["Trip/b.png", "a.png"]);
    }

    #[test]
    fn key_changes_with_the_file_and_the_bar_colour() {
        let k = key_for("a.jpg", 10, 5, [255; 3]);
        assert_ne!(k, key_for("a.jpg", 11, 5, [255; 3]));
        assert_ne!(k, key_for("a.jpg", 10, 6, [255; 3]));
        assert_ne!(k, key_for("a.jpg", 10, 5, [0; 3]));
        assert_eq!(k, key_for("a.jpg", 10, 5, [255; 3]));
    }

    #[test]
    fn process_then_load_and_clean_up() {
        let photos = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        png(&photos.path().join("a.png"), 400, 300);
        let mut lib = Library::new(photos.path(), state.path(), [255; 3], Limits::default());
        let snap = lib.rescan();
        assert_eq!(snap.pending.len(), 1);
        assert!(snap.ready.is_empty());
        let item = snap.pending[0].clone();
        lib.process(&item).unwrap();
        assert_eq!(lib.ready_keys(), vec![item.key.clone()]);
        let snap = lib.rescan();
        assert!(snap.pending.is_empty());
        assert_eq!(snap.ready, vec![item.key.clone()]);
        assert!(snap.stale.is_empty());
        let p = lib.load(&item.key).unwrap();
        assert_eq!(p.rgb.len(), DISPLAY_W * DISPLAY_H * 3);
        std::fs::write(lib.cache_dir().join("stale.jpg"), b"x").unwrap();
        std::fs::write(lib.cache_dir().join("left.tmp"), b"x").unwrap();
        let snap = lib.rescan();
        assert_eq!(snap.stale.len(), 2);
        lib.remove_stale(&snap.stale);
        assert!(!lib.cache_dir().join("stale.jpg").exists() && !lib.cache_dir().join("left.tmp").exists());
        assert!(lib.is_ready(&item.key), "the real copy stays");
    }

    #[test]
    fn a_file_still_being_written_is_not_settled_until_unchanged() {
        let photos = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut lib = Library::new(photos.path(), state.path(), [255; 3], Limits::default());
        assert!(lib.rescan().pending.is_empty(), "empty folder at startup");
        png(&photos.path().join("new.png"), 10, 10);
        assert!(lib.rescan().pending.is_empty(), "first sighting after startup: not yet");
        assert_eq!(lib.rescan().pending.len(), 1, "unchanged on the next scan");
    }

    #[test]
    fn asking_for_ready_photos_does_not_make_a_new_file_look_settled() {
        let photos = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut lib = Library::new(photos.path(), state.path(), [255; 3], Limits::default());
        lib.rescan();
        png(&photos.path().join("new.png"), 10, 10);
        // Pretend its fitted copy already exists, so only "settled" decides.
        let item = scan(photos.path(), [255; 3]).remove(0);
        std::fs::create_dir_all(lib.cache_dir()).unwrap();
        std::fs::write(lib.cache_dir().join(format!("{}.jpg", item.key)), b"x").unwrap();
        for _ in 0..3 {
            assert!(lib.ready_keys().is_empty(), "no rescan in between: still unsettled");
        }
        assert!(lib.rescan().ready.is_empty(), "first sighting");
        assert_eq!(lib.ready_keys().len(), 1, "now settled");
    }

    #[test]
    fn broken_photos_are_not_retried() {
        let photos = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(photos.path().join("bad.jpg"), b"not a jpeg").unwrap();
        let mut lib = Library::new(photos.path(), state.path(), [255; 3], Limits::default());
        let item = lib.pending().remove(0);
        assert!(lib.process(&item).is_err());
        lib.failed_handle().lock().unwrap().insert(item.key.clone());
        assert!(lib.pending().is_empty());
    }

    #[test]
    fn an_unreadable_cache_copy_can_be_discarded_so_it_is_decoded_again() {
        let photos = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        png(&photos.path().join("a.png"), 400, 300);
        let mut lib = Library::new(photos.path(), state.path(), [255; 3], Limits::default());
        let item = lib.pending().remove(0);
        std::fs::create_dir_all(lib.cache_dir()).unwrap();
        std::fs::write(lib.cache_dir().join(format!("{}.jpg", item.key)), b"").unwrap(); // a power cut left it empty
        assert!(lib.load(&item.key).is_err());
        lib.discard(&item.key);
        assert_eq!(lib.rescan().pending.len(), 1);
    }
}
