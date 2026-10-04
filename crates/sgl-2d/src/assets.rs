//! Asset loading and caching: native clients can read an asset directory;
//! browser bootstrap supplies the same bytes as an [`AssetBundle`]. GPU upload
//! stays in the renderer.
//!
//! This layer owns the **decoded** [`Texture`]
//! (CPU RGBA8 bytes); the renderer owns the GPU upload, keyed by the
//! [`Handle`]. Loading and decoding are synchronous.
//!
//! [`white_texture`] registers the one 1×1 white pixel every flat-quad
//! consumer shares (UI rects, overlay lines, screen fades) under a synthetic
//! path, so they all resolve to the same handle and the same GPU upload.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

/// Default asset root, relative to the working directory.
const ASSET_ROOT: &str = "assets";

/// Synthetic asset path of the shared 1×1 white texture. Not a file: it names
/// a slot in [`Assets`] so every consumer of [`white_texture`] shares one
/// handle (and therefore one GPU upload and one sprite batch).
pub const WHITE_TEXTURE_PATH: &str = "sgl://white";

/// A decoded CPU-side texture: straight-alpha RGBA8 rows, top-to-bottom.
///
/// Pixel data only — no GPU state. The renderer uploads it (sRGB format,
/// nearest sampling) and afterwards only the [`Handle`] circulates.
pub struct Texture {
    pub width: u32,
    pub height: u32,
    /// `width × height × 4` bytes, row-major RGBA, straight (non-premultiplied)
    /// alpha exactly as decoded from the PNG.
    pub rgba: Vec<u8>,
}

impl Texture {
    /// Decode a PNG (or any enabled format) from `path` into RGBA8.
    pub fn load(path: &Path) -> Result<Self, AssetError> {
        let img = image::open(path)
            .map_err(|source| AssetError::Image {
                path: path.to_owned(),
                source,
            })?
            .into_rgba8();
        Ok(Self::from_rgba(img))
    }

    /// Decode a PNG from already-loaded bytes (browser `AssetBundle`).
    pub fn decode(path: &Path, bytes: &[u8]) -> Result<Self, AssetError> {
        let img = image::load_from_memory(bytes)
            .map_err(|source| AssetError::Image {
                path: path.to_owned(),
                source,
            })?
            .into_rgba8();
        Ok(Self::from_rgba(img))
    }

    fn from_rgba(img: image::RgbaImage) -> Self {
        let (width, height) = img.dimensions();
        Self {
            width,
            height,
            rgba: img.into_raw(),
        }
    }
}

/// Recoverable asset-loading errors. Content failures return this and are
/// logged + skipped by callers; they never panic.
#[derive(Debug)]
pub enum AssetError {
    /// A logical path was absent from an in-memory runtime bundle.
    Missing { path: PathBuf },
    /// Reading native source bytes failed.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Reading or decoding an image failed.
    Image {
        path: PathBuf,
        source: image::ImageError,
    },
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { path } => write!(f, "asset missing from bundle: {}", path.display()),
            Self::Io { path, source } => write!(f, "asset read {}: {source}", path.display()),
            Self::Image { path, source } => {
                write!(f, "image load {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for AssetError {}

/// Complete browser startup bundle keyed by asset-relative logical path.
#[derive(Debug, Clone, Default)]
pub struct AssetBundle {
    files: HashMap<String, Vec<u8>>,
}

impl AssetBundle {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert one canonical path. Returns the old value when duplicated so
    /// manifest loaders can reject collisions explicitly.
    pub fn insert(&mut self, path: impl Into<String>, bytes: Vec<u8>) -> Option<Vec<u8>> {
        self.files.insert(path.into(), bytes)
    }

    #[must_use]
    pub fn get(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(Vec::as_slice)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// A cache key for an asset of type `T`: the index of its slot in
/// [`Assets`].
///
/// [`Assets`] never removes an asset, so a handle stays valid for its
/// cache's lifetime. `Copy`/`Eq`/`Hash` regardless of `T`, so handles travel
/// freely through draw lists and renderer maps.
pub struct Handle<T> {
    index: usize,
    _marker: PhantomData<fn() -> T>,
}

// Manual impls so `Handle<T>` is Copy/Eq/Hash regardless of whether `T` is.
impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Handle<T> {}
impl<T> PartialEq for Handle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}
impl<T> Eq for Handle<T> {}
impl<T> std::hash::Hash for Handle<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.index.hash(state);
    }
}
impl<T> std::fmt::Debug for Handle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let full = std::any::type_name::<T>();
        let short = full.rsplit("::").next().unwrap_or(full);
        write!(f, "Handle<{short}>({})", self.index)
    }
}

impl<T> Handle<T> {
    fn new(index: usize) -> Self {
        Self {
            index,
            _marker: PhantomData,
        }
    }
}

/// A path-keyed cache of assets of type `T`.
///
/// Loading dedupes by path (the same path returns the same handle). Slots are
/// stable, so a handle stays valid for the cache's lifetime.
pub struct Assets<T> {
    slots: Vec<T>,
    by_path: HashMap<PathBuf, usize>,
}

impl<T> Assets<T> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            by_path: HashMap::new(),
        }
    }

    /// Insert `value` for `path`, returning its handle. If `path` is already
    /// cached, replaces the value in place (the handle stays stable) and
    /// returns the existing handle.
    pub fn insert(&mut self, path: PathBuf, value: T) -> Handle<T> {
        if let Some(&index) = self.by_path.get(&path) {
            self.slots[index] = value;
            return Handle::new(index);
        }
        let index = self.slots.len();
        self.slots.push(value);
        self.by_path.insert(path, index);
        Handle::new(index)
    }

    /// Existing handle for `path`, if cached.
    #[must_use]
    pub fn handle_for(&self, path: &Path) -> Option<Handle<T>> {
        self.by_path.get(path).map(|&index| Handle::new(index))
    }

    /// Borrow the asset behind `handle`, or `None` for a handle this cache
    /// did not issue.
    #[must_use]
    pub fn get(&self, handle: Handle<T>) -> Option<&T> {
        self.slots.get(handle.index)
    }
}

impl<T> Default for Assets<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// The shared 1×1 opaque-white texture, registered on first call and fetched
/// on every later one — the source of the flat quads that UI rects, overlay
/// lines and screen fades scale up ([`crate::canvas::overlay`]).
///
/// Idempotent: the handle is stable for the cache's lifetime, so callers may
/// call it freely. The texture still has to reach the GPU once; a consumer
/// with a renderer should use `Renderer::white_texture`, which registers
/// through this function *and* uploads.
pub fn white_texture(assets: &mut Assets<Texture>) -> Handle<Texture> {
    let path = Path::new(WHITE_TEXTURE_PATH);
    if let Some(handle) = assets.handle_for(path) {
        return handle;
    }
    assets.insert(
        path.to_path_buf(),
        Texture {
            width: 1,
            height: 1,
            rgba: vec![255; 4],
        },
    )
}

/// The synchronous asset server: caches textures, deduplicated by path.
pub struct AssetServer {
    pub textures: Assets<Texture>,
    root: PathBuf,
    bundle: Option<AssetBundle>,
}

impl AssetServer {
    /// A server rooted at the default `assets/` directory (cwd-relative).
    #[must_use]
    pub fn new() -> Self {
        Self::with_root(PathBuf::from(ASSET_ROOT))
    }

    /// A server rooted at an explicit directory (tests, tools).
    #[must_use]
    pub fn with_root(root: PathBuf) -> Self {
        Self {
            textures: Assets::new(),
            root,
            bundle: None,
        }
    }

    /// A synchronous cache over a complete browser startup bundle.
    #[must_use]
    pub fn from_bundle(bundle: AssetBundle) -> Self {
        Self {
            textures: Assets::new(),
            root: PathBuf::from(ASSET_ROOT),
            bundle: Some(bundle),
        }
    }

    /// Read one asset's source bytes from the bundle or native filesystem.
    pub fn load_bytes(&self, rel: &str) -> Result<Vec<u8>, AssetError> {
        if let Some(bundle) = &self.bundle {
            return bundle
                .get(rel)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| AssetError::Missing {
                    path: PathBuf::from(rel),
                });
        }
        let full = self.resolve(rel);
        std::fs::read(&full).map_err(|source| AssetError::Io { path: full, source })
    }

    /// Resolve an asset-relative path (e.g. `"player/common/shadow.png"`)
    /// under the root.
    #[must_use]
    pub fn resolve(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Load (or fetch cached) a texture by asset-relative path.
    pub fn load_texture(&mut self, rel: &str) -> Result<Handle<Texture>, AssetError> {
        let full = self.resolve(rel);
        if let Some(h) = self.textures.handle_for(&full) {
            return Ok(h);
        }
        let texture = if let Some(bundle) = &self.bundle {
            let bytes = bundle.get(rel).ok_or_else(|| AssetError::Missing {
                path: PathBuf::from(rel),
            })?;
            Texture::decode(&full, bytes)?
        } else {
            Texture::load(&full)?
        };
        Ok(self.textures.insert(full, texture))
    }
}

impl Default for AssetServer {
    fn default() -> Self {
        Self::new()
    }
}

/// Browser-only HTTP loader for the generated runtime manifest (D-17).
#[cfg(target_arch = "wasm32")]
pub async fn fetch_runtime_bundle(manifest_url: &str) -> Result<AssetBundle, String> {
    use futures::future::try_join_all;
    use js_sys::Uint8Array;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::Response;

    async fn response(url: &str) -> Result<Response, String> {
        let window = web_sys::window().ok_or_else(|| "browser window unavailable".to_owned())?;
        let value = JsFuture::from(window.fetch_with_str(url))
            .await
            .map_err(|error| format!("fetch {url}: {error:?}"))?;
        let response: Response = value
            .dyn_into()
            .map_err(|_| format!("fetch {url}: response had the wrong type"))?;
        if !response.ok() {
            return Err(format!("fetch {url}: HTTP {}", response.status()));
        }
        Ok(response)
    }

    let manifest_response = response(manifest_url).await?;
    let manifest = JsFuture::from(
        manifest_response
            .text()
            .map_err(|error| format!("read {manifest_url}: {error:?}"))?,
    )
    .await
    .map_err(|error| format!("read {manifest_url}: {error:?}"))?
    .as_string()
    .ok_or_else(|| format!("read {manifest_url}: body was not text"))?;

    let paths: Vec<String> = manifest
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    let mut unique = std::collections::HashSet::with_capacity(paths.len());
    if paths.iter().any(|path| !unique.insert(path.clone())) {
        return Err("runtime asset manifest contains duplicate paths".to_owned());
    }

    let fetched = try_join_all(paths.into_iter().map(|path| async move {
        let url = format!("assets/{path}");
        let response = response(&url).await?;
        let buffer = JsFuture::from(
            response
                .array_buffer()
                .map_err(|error| format!("read {url}: {error:?}"))?,
        )
        .await
        .map_err(|error| format!("read {url}: {error:?}"))?;
        Ok::<_, String>((path, Uint8Array::new(&buffer).to_vec()))
    }))
    .await?;

    let mut bundle = AssetBundle::new();
    for (path, bytes) in fetched {
        if bundle.insert(path, bytes).is_some() {
            return Err("runtime asset manifest resolved a duplicate path".to_owned());
        }
    }
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #255: bundle bookkeeping and handle hashing behave as keys.
    #[wasm_bindgen_test(unsupported = test)]
    fn bundle_counts_entries_and_handles_hash_by_identity() {
        let mut bundle = AssetBundle::new();
        assert!(bundle.is_empty());
        assert_eq!(bundle.len(), 0);
        bundle.insert("a.png", vec![1]);
        bundle.insert("b.png", vec![2]);
        assert!(!bundle.is_empty());
        assert_eq!(bundle.len(), 2);

        let mut assets: Assets<u32> = Assets::new();
        let a = assets.insert(PathBuf::from("a"), 1);
        let b = assets.insert(PathBuf::from("b"), 2);
        let mut map = std::collections::HashMap::new();
        map.insert(a, "a");
        map.insert(b, "b");
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get(&assets.handle_for(Path::new("a")).unwrap()),
            Some(&"a")
        );
    }

    fn fixture_png() -> Vec<u8> {
        let pixels = image::RgbaImage::from_pixel(2, 3, image::Rgba([10, 20, 30, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .expect("encode fixture PNG");
        bytes.into_inner()
    }

    /// Same path → same handle; distinct paths → distinct handles.
    #[wasm_bindgen_test(unsupported = test)]
    fn cache_dedupes_by_path() {
        let mut assets: Assets<u32> = Assets::new();
        let a = assets.insert(PathBuf::from("a.png"), 1);
        let b = assets.insert(PathBuf::from("b.png"), 2);
        assert_ne!(a, b);
        assert_eq!(assets.handle_for(Path::new("a.png")), Some(a));
        assert_eq!(assets.get(a), Some(&1));
        assert_eq!(assets.get(b), Some(&2));
        // Re-insert replaces in place, handle stable.
        let a2 = assets.insert(PathBuf::from("a.png"), 9);
        assert_eq!(a, a2);
        assert_eq!(assets.get(a), Some(&9));
    }

    /// Loading a PNG from a native asset root decodes RGBA8 and deduplicates.
    #[cfg(not(target_arch = "wasm32"))]
    #[wasm_bindgen_test(unsupported = test)]
    fn loads_real_png_from_assets() {
        let root = crate::test_fs::TempDir::new();
        root.write("pixel.png", &fixture_png());
        let mut server = AssetServer::with_root(root.path().to_path_buf());
        let handle = server
            .load_texture("pixel.png")
            .expect("fixture PNG should decode");
        let tex = server.textures.get(handle).unwrap();
        assert_eq!((tex.width, tex.height), (2, 3));
        assert_eq!(tex.rgba.len(), 2 * 3 * 4);
        let again = server.load_texture("pixel.png").unwrap();
        assert_eq!(handle, again);
    }

    /// One canonical registration: repeat calls hand back the same handle
    /// over one 1×1 opaque-white pixel, so every flat-quad consumer shares a
    /// single slot (and a single GPU upload).
    #[wasm_bindgen_test(unsupported = test)]
    fn white_texture_registers_one_shared_opaque_pixel() {
        let mut assets: Assets<Texture> = Assets::new();
        let first = white_texture(&mut assets);
        let second = white_texture(&mut assets);
        assert_eq!(first, second);
        let tex = assets.get(first).expect("the white pixel stays registered");
        assert_eq!((tex.width, tex.height), (1, 1));
        assert_eq!(tex.rgba, vec![255, 255, 255, 255]);
        // A distinct asset must not collide with the synthetic path.
        let other = assets.insert(
            PathBuf::from("other.png"),
            Texture {
                width: 1,
                height: 1,
                rgba: vec![0; 4],
            },
        );
        assert_ne!(first, other);
    }

    /// A missing file is a recoverable error, not a panic.
    #[wasm_bindgen_test(unsupported = test)]
    fn missing_file_is_an_error() {
        let mut server = AssetServer::with_root(PathBuf::from("/nonexistent-root"));
        assert!(server.load_texture("nope.png").is_err());
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn bundle_uses_the_same_decoder_and_rejects_missing_paths() {
        let mut bundle = AssetBundle::new();
        assert!(bundle.insert("pixel.png", fixture_png()).is_none());
        let mut server = AssetServer::from_bundle(bundle);
        let handle = server.load_texture("pixel.png").unwrap();
        let texture = server.textures.get(handle).unwrap();
        assert_eq!((texture.width, texture.height), (2, 3));
        assert!(matches!(
            server.load_texture("missing.png"),
            Err(AssetError::Missing { .. })
        ));
    }
}
