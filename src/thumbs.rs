//! The thumbnail grid's pictures: which files get one, loading them in the
//! background, and a cache that keeps them within a memory budget.
//!
//! Images are decoded with the same helper the Details preview uses (a photo's
//! embedded EXIF thumbnail first, so a 40-megapixel JPEG is not decoded just to
//! be shrunk), and STL/OBJ models are rendered by the software rasterizer the
//! model viewer uses. Both run on the blocking pool, a few at a time.

use crate::config::ThumbSize;
use crate::vfs::{Vfs, VfsEntry, VfsPath};
use image::RgbaImage;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::io::AsyncReadExt;

/// Decoded pixels kept across all cached thumbnails of a panel.
const BUDGET: usize = 64 * 1024 * 1024;
/// Largest image read for a thumbnail from a local disk, and over a remote
/// connection or out of an archive, where every byte is a transfer.
const IMAGE_MAX: u64 = 30 * 1024 * 1024;
const IMAGE_MAX_REMOTE: u64 = 8 * 1024 * 1024;
/// Largest model file rendered, and the most triangles: a thumbnail is a glance,
/// not worth seconds of rasterizing a scan with millions of faces.
const MODEL_MAX: u64 = 16 * 1024 * 1024;
const MODEL_MAX_TRIS: usize = 150_000;
/// Thumbnails loaded at once.
pub const PARALLEL: usize = 4;

/// What kind of picture a file gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Image,
    Model,
}

/// The picture kind for `e`, if it gets one. A model needs its file on the
/// local disk: rendering one pulled over a connection is not worth the wait.
pub fn kind_of(e: &VfsEntry, dir: &VfsPath) -> Option<Kind> {
    if !matches!(e.kind, crate::vfs::VfsKind::File | crate::vfs::VfsKind::Symlink) {
        return None;
    }
    if crate::util::img::is_image_name(&e.name) {
        let cap = if dir.is_plain_local() { IMAGE_MAX } else { IMAGE_MAX_REMOTE };
        return (e.size <= cap).then_some(Kind::Image);
    }
    if crate::mesh::is_model_name(&e.name) && dir.is_plain_local() {
        return (e.size <= MODEL_MAX).then_some(Kind::Model);
    }
    None
}

/// A decoded thumbnail.
#[derive(Debug)]
pub struct Thumb {
    pub img: RgbaImage,
    pub sig: u64,
}

/// What identifies a thumbnail: the file as it is now, at the size asked for,
/// and — for a model, rendered onto the panel's colour — the background.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThumbKey {
    pub path: String,
    pub size: u64,
    pub mtime: Option<SystemTime>,
    pub px: u32,
    pub bg: (u8, u8, u8),
}

impl ThumbKey {
    pub fn new(dir: &VfsPath, e: &VfsEntry, size: ThumbSize, bg: (u8, u8, u8)) -> Self {
        let model = crate::mesh::is_model_name(&e.name);
        ThumbKey {
            path: dir.join(&e.name).display(),
            size: e.size,
            mtime: e.mtime,
            px: size.pixels(),
            bg: if model { bg } else { (0, 0, 0) },
        }
    }
}

pub enum ThumbState {
    Loading,
    Ready(Arc<Thumb>),
    Failed,
}

/// One panel's thumbnails.
pub struct ThumbCache {
    entries: HashMap<ThumbKey, (ThumbState, u64)>,
    /// Bytes of decoded pixels held.
    bytes: usize,
    clock: u64,
    /// The directory being shown; leaving it cancels what is still loading.
    pub dir: String,
    pub cancel: crate::ops::CancelToken,
    /// The cell size the grid is drawn at, kept in step with the setting.
    pub size: ThumbSize,
}

impl Default for ThumbCache {
    fn default() -> Self {
        ThumbCache {
            entries: HashMap::new(),
            bytes: 0,
            clock: 0,
            dir: String::new(),
            cancel: crate::ops::CancelToken::new(),
            size: ThumbSize::default(),
        }
    }
}

impl ThumbCache {
    /// The state of `key`, marking it as recently used.
    pub fn get(&mut self, key: &ThumbKey) -> Option<&ThumbState> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(key).map(|(state, used)| {
            *used = clock;
            &*state
        })
    }

    pub fn contains(&self, key: &ThumbKey) -> bool {
        self.entries.contains_key(key)
    }

    pub fn start(&mut self, key: ThumbKey) {
        self.clock += 1;
        self.entries.insert(key, (ThumbState::Loading, self.clock));
    }

    /// A load finished. Kept only if it is still wanted — the directory may
    /// have been left, which forgets what was loading.
    pub fn finish(&mut self, key: ThumbKey, thumb: Option<Arc<Thumb>>) {
        let Some((state, _)) = self.entries.get_mut(&key) else { return };
        if !matches!(state, ThumbState::Loading) {
            return;
        }
        *state = match thumb {
            Some(t) => {
                self.bytes += t.img.as_raw().len();
                ThumbState::Ready(t)
            }
            None => ThumbState::Failed,
        };
        self.evict();
    }

    /// Show another directory: stop loading for the old one. Thumbnails that
    /// are done stay, since coming back is common.
    pub fn set_dir(&mut self, dir: String) {
        if dir == self.dir {
            return;
        }
        self.dir = dir;
        self.cancel.cancel();
        self.cancel = crate::ops::CancelToken::new();
        self.entries.retain(|_, (state, _)| !matches!(state, ThumbState::Loading));
    }

    /// Drop the least recently used thumbnails until the pixels fit the budget.
    fn evict(&mut self) {
        while self.bytes > BUDGET {
            let oldest = self
                .entries
                .iter()
                .filter(|(_, (s, _))| matches!(s, ThumbState::Ready(_)))
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| k.clone());
            let Some(key) = oldest else { break };
            if let Some((ThumbState::Ready(t), _)) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(t.img.as_raw().len());
            }
        }
    }

    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

/// Load one thumbnail: read the file, decode or render it, and shrink it to
/// `px` on its longest edge. `None` when it doesn't decode or is too big.
pub async fn load(
    backend: Arc<dyn Vfs>,
    path: VfsPath,
    name: String,
    kind: Kind,
    px: u32,
    bg: (u8, u8, u8),
    base: (u8, u8, u8),
) -> Option<Arc<Thumb>> {
    let cap = match kind {
        Kind::Image if path.is_plain_local() => IMAGE_MAX,
        Kind::Image => IMAGE_MAX_REMOTE,
        Kind::Model => MODEL_MAX,
    };
    let reader = backend.open_read(&path).await.ok()?;
    let mut bytes = Vec::new();
    reader.take(cap + 1).read_to_end(&mut bytes).await.ok()?;
    if bytes.len() as u64 > cap {
        return None;
    }
    let img = tokio::task::spawn_blocking(move || match kind {
        Kind::Image => crate::util::img::decode_scaled(&bytes, px, true),
        Kind::Model => render_model(&bytes, &name, px, bg, base),
    })
    .await
    .ok()??;
    let sig = crate::util::img::image_sig(&img);
    Some(Arc::new(Thumb { img, sig }))
}

/// A model, framed and lit the way the viewer first shows it, as a `px`-wide
/// picture a little wider than tall.
fn render_model(
    bytes: &[u8],
    name: &str,
    px: u32,
    bg: (u8, u8, u8),
    base: (u8, u8, u8),
) -> Option<RgbaImage> {
    let mesh = crate::mesh::load(bytes, name)?;
    if mesh.tris.len() > MODEL_MAX_TRIS {
        return None;
    }
    let (cam, _) = mesh.framed_camera();
    let img = crate::space3d::raster3d::render_mesh(
        px,
        px * 4 / 5,
        &mesh.tris,
        mesh.min.y,
        cam.eye(),
        cam.target,
        bg,
        base,
    );
    Some(crop_to_content(img, bg))
}

/// Cut away the background around what was drawn, keeping a small margin. The
/// camera frames a model's bounding sphere, which leaves a flat or long one
/// small in the middle of its picture — fine in the viewer, where it can be
/// zoomed, but a waste of a thumbnail's few cells.
fn crop_to_content(img: RgbaImage, bg: (u8, u8, u8)) -> RgbaImage {
    let differs = |p: &image::Rgba<u8>| {
        let d = |a: u8, b: u8| a.abs_diff(b) > 6;
        d(p[0], bg.0) || d(p[1], bg.1) || d(p[2], bg.2)
    };
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, p) in img.enumerate_pixels() {
        if differs(p) {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x0 > x1 || y0 > y1 {
        return img;
    }
    let pad = (x1 - x0).max(y1 - y0) / 16 + 1;
    let (x0, y0) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
    let (x1, y1) = ((x1 + pad).min(img.width() - 1), (y1 + pad).min(img.height() - 1));
    image::imageops::crop_imm(&img, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(path: &str) -> ThumbKey {
        ThumbKey { path: path.into(), size: 1, mtime: None, px: 100, bg: (0, 0, 0) }
    }

    fn thumb(w: u32, h: u32) -> Arc<Thumb> {
        Arc::new(Thumb { img: RgbaImage::new(w, h), sig: w as u64 })
    }

    #[test]
    fn the_least_recently_seen_thumbnails_go_first_when_over_budget() {
        let mut cache = ThumbCache::default();
        // Each 2048×2048 RGBA picture is 16 MiB: four fit the budget.
        for i in 0..4 {
            cache.start(key(&format!("/p/{i}")));
            cache.finish(key(&format!("/p/{i}")), Some(thumb(2048, 2048)));
        }
        assert_eq!(cache.bytes(), BUDGET);
        cache.get(&key("/p/0")); // looked at again: keep it
        cache.start(key("/p/4"));
        cache.finish(key("/p/4"), Some(thumb(2048, 2048)));
        assert!(cache.bytes() <= BUDGET);
        assert!(cache.contains(&key("/p/0")), "recently seen");
        assert!(!cache.contains(&key("/p/1")), "the oldest went");
    }

    #[test]
    fn leaving_a_directory_forgets_what_was_still_loading() {
        let mut cache = ThumbCache::default();
        cache.set_dir("/a".into());
        cache.start(key("/a/done.png"));
        cache.finish(key("/a/done.png"), Some(thumb(4, 4)));
        cache.start(key("/a/slow.png"));
        let cancel = cache.cancel.clone();
        cache.set_dir("/b".into());
        assert!(cancel.is_cancelled());
        assert!(cache.contains(&key("/a/done.png")), "finished ones stay for coming back");
        assert!(!cache.contains(&key("/a/slow.png")));
        cache.finish(key("/a/slow.png"), Some(thumb(4, 4)));
        assert!(!cache.contains(&key("/a/slow.png")), "a late result is not wanted");
    }

    #[tokio::test]
    async fn images_and_models_get_thumbnails_and_other_files_do_not() {
        let dir = std::env::temp_dir().join(format!("rc_thumbs_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::from_pixel(400, 200, image::Rgba([10, 200, 30, 255]))
            .save(dir.join("wide.png"))
            .unwrap();
        let stl = "solid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid t\n";
        std::fs::write(dir.join("part.stl"), stl).unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();

        let registry = crate::vfs::registry::Registry::new();
        let backend = registry.local();
        let cwd = VfsPath::local(&dir);
        let entries = backend.read_dir(&cwd).await.unwrap();
        let entry = |n: &str| entries.iter().find(|e| e.name == n).unwrap().clone();
        assert_eq!(kind_of(&entry("wide.png"), &cwd), Some(Kind::Image));
        assert_eq!(kind_of(&entry("part.stl"), &cwd), Some(Kind::Model));
        assert_eq!(kind_of(&entry("notes.txt"), &cwd), None);
        let remote = VfsPath { scheme: "sftp-1".into(), path: dir.clone(), container: None };
        assert_eq!(kind_of(&entry("part.stl"), &remote), None, "no models over a connection");

        let load_one = |name: &str, kind| {
            load(
                backend.clone(),
                cwd.join(name),
                name.into(),
                kind,
                100,
                (0, 0, 80),
                (230, 230, 230),
            )
        };
        let t = load_one("wide.png", Kind::Image).await.expect("an image thumbnail");
        assert_eq!((t.img.width(), t.img.height()), (100, 50), "shrunk, aspect kept");
        let m = load_one("part.stl", Kind::Model).await.expect("a model thumbnail");
        assert!(m.img.width() <= 100 && m.img.height() <= 80, "at most the rendered size");
        let edge = m.img.get_pixel(0, 0);
        assert!(
            m.img.width() < 100 || m.img.height() < 80 || (edge[0], edge[1], edge[2]) != (0, 0, 80),
            "cropped to the model"
        );
        assert!(load_one("notes.txt", Kind::Image).await.is_none(), "not an image after all");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
