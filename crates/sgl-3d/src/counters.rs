//! What the library counts on each thread while the `diagnostics` feature is
//! on (`diagnostics::counters`): its uploads by call site, the buffers it
//! creates, the time each step of building a model and each
//! instance BVH takes, the ray source's and geometry slabs' growths and the
//! static-edit boxes recorded and merged. Without the feature every function here is a
//! pass-through and nothing is kept.

/// A step of preparing or placing a model, or of building an instance BVH,
/// which `Counters::steps` times. Preparing a model (`PreparedModel::new`)
/// runs `Validate`, `Pack`, `RayBvh`, `Ranges` and `Clusters` on the
/// thread that prepares it; `Scene::add_model` and `set_model` run `Place`
/// and `Write` on theirs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildStep {
    /// Checking a mesh's indices, positions and deformation.
    Validate,
    /// Packing a model's ray-source words (mesh records, vertices, indices,
    /// a deforming model's influences and morph targets) and its meshes'
    /// shadow-caster positions.
    Pack,
    /// Building a model's BVH.
    RayBvh,
    /// A mesh's culling hierarchy.
    Ranges,
    /// A mesh's local-light shadow caster clusters.
    Clusters,
    /// Placing a prepared model: checking it against the scene and the
    /// device, allocating its ranges (growing a buffer if it must) and
    /// rebasing the words that address them.
    Place,
    /// Copying a placed model's words and geometry to the queue.
    Write,
    /// Building the static instance BVH (after a static edit).
    StaticInstanceBvh,
    /// Building the moving instance BVH (every traced frame).
    MovingInstanceBvh,
}

/// One call site's uploads: what the library wrote to the GPU from that
/// line through the queue or a buffer created with contents. A site is its
/// file and line, which move as the library is edited: compare versions by
/// `Counters::uploaded_bytes` or by sums over a file, not by line.
#[cfg(any(test, feature = "diagnostics"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadSite {
    pub file: &'static str,
    pub line: u32,
    pub bytes: u64,
    pub writes: u64,
}

/// One build step's calls and time; in the browser, the time is
/// `performance.now()`'s, which the browser coarsens.
#[cfg(any(test, feature = "diagnostics"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepTime {
    pub step: BuildStep,
    pub calls: u64,
    pub nanoseconds: u64,
}

/// What this thread counted since it started (`diagnostics::counters`).
/// Subtract two of them for what happened between.
#[cfg(any(test, feature = "diagnostics"))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Uploads by call site, in the order the sites first uploaded.
    pub uploads: Vec<UploadSite>,
    /// Buffers created, with contents or without.
    pub buffers_created: u64,
    /// Build steps, in the order they first ran.
    pub steps: Vec<StepTime>,
    /// Growths of the ray source, each a copy of it whole.
    pub ray_source_growths: u64,
    /// Growths of a geometry slab, each a copy of it whole.
    pub geometry_growths: u64,
    /// Static-edit boxes recorded, and those merged into another: the scene
    /// keeps at most 1024 pending, and past them halves the list, merging
    /// 512 pairs at once.
    pub static_edit_boxes: u64,
    pub static_edit_boxes_merged: u64,
}

#[cfg(any(test, feature = "diagnostics"))]
impl Counters {
    /// The bytes uploaded from every site.
    pub fn uploaded_bytes(&self) -> u64 {
        self.uploads.iter().map(|site| site.bytes).sum()
    }

    /// `step`'s calls and time.
    pub fn step(&self, step: BuildStep) -> StepTime {
        self.steps
            .iter()
            .find(|time| time.step == step)
            .copied()
            .unwrap_or(StepTime {
                step,
                calls: 0,
                nanoseconds: 0,
            })
    }

    /// What was counted after `earlier`, a snapshot of the same thread.
    pub fn since(&self, earlier: &Counters) -> Counters {
        let uploads = self
            .uploads
            .iter()
            .filter_map(|site| {
                let before = earlier
                    .uploads
                    .iter()
                    .find(|old| old.line == site.line && old.file == site.file);
                let bytes = site.bytes - before.map_or(0, |old| old.bytes);
                let writes = site.writes - before.map_or(0, |old| old.writes);
                (writes > 0).then(|| UploadSite {
                    bytes,
                    writes,
                    ..site.clone()
                })
            })
            .collect();
        let steps = self
            .steps
            .iter()
            .map(|time| {
                let before = earlier.step(time.step);
                StepTime {
                    calls: time.calls - before.calls,
                    nanoseconds: time.nanoseconds - before.nanoseconds,
                    ..*time
                }
            })
            .filter(|time| time.calls > 0)
            .collect();
        Counters {
            uploads,
            buffers_created: self.buffers_created - earlier.buffers_created,
            steps,
            ray_source_growths: self.ray_source_growths - earlier.ray_source_growths,
            geometry_growths: self.geometry_growths - earlier.geometry_growths,
            static_edit_boxes: self.static_edit_boxes - earlier.static_edit_boxes,
            static_edit_boxes_merged: self.static_edit_boxes_merged
                - earlier.static_edit_boxes_merged,
        }
    }
}

#[cfg(any(test, feature = "diagnostics"))]
thread_local! {
    static COUNTERS: std::cell::RefCell<Counters> = std::cell::RefCell::default();
}

/// Counts `bytes` written by one upload at the caller's call site.
#[cfg(any(test, feature = "diagnostics"))]
#[cfg_attr(any(test, feature = "diagnostics"), track_caller)]
fn upload(bytes: usize) {
    let at = std::panic::Location::caller();
    COUNTERS.with_borrow_mut(|counters| {
        let at_site = |site: &UploadSite| site.line == at.line() && site.file == at.file();
        if !counters.uploads.iter().any(at_site) {
            counters.uploads.push(UploadSite {
                file: at.file(),
                line: at.line(),
                bytes: 0,
                writes: 0,
            });
        }
        let site = counters
            .uploads
            .iter_mut()
            .find(|site| at_site(site))
            .unwrap();
        site.bytes += bytes as u64;
        site.writes += 1;
    });
}

/// `queue.write_buffer`, counted.
#[cfg_attr(any(test, feature = "diagnostics"), track_caller)]
pub(crate) fn write_buffer(queue: &wgpu::Queue, buffer: &wgpu::Buffer, offset: u64, data: &[u8]) {
    #[cfg(any(test, feature = "diagnostics"))]
    upload(data.len());
    queue.write_buffer(buffer, offset, data);
}

/// `queue.write_texture`, counted.
#[cfg_attr(any(test, feature = "diagnostics"), track_caller)]
pub(crate) fn write_texture(
    queue: &wgpu::Queue,
    texture: wgpu::TexelCopyTextureInfo<'_>,
    data: &[u8],
    layout: wgpu::TexelCopyBufferLayout,
    size: wgpu::Extent3d,
) {
    #[cfg(any(test, feature = "diagnostics"))]
    upload(data.len());
    queue.write_texture(texture, data, layout, size);
}

/// `device.create_buffer`, counted as a buffer created.
#[cfg_attr(any(test, feature = "diagnostics"), track_caller)]
pub(crate) fn buffer(
    device: &wgpu::Device,
    descriptor: &wgpu::BufferDescriptor<'_>,
) -> wgpu::Buffer {
    #[cfg(any(test, feature = "diagnostics"))]
    COUNTERS.with_borrow_mut(|counters| counters.buffers_created += 1);
    device.create_buffer(descriptor)
}

/// `device.create_buffer_init`, counted as an upload and a buffer created.
#[cfg_attr(any(test, feature = "diagnostics"), track_caller)]
pub(crate) fn buffer_init(
    device: &wgpu::Device,
    descriptor: &wgpu::util::BufferInitDescriptor<'_>,
) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    #[cfg(any(test, feature = "diagnostics"))]
    {
        upload(descriptor.contents.len());
        COUNTERS.with_borrow_mut(|counters| counters.buffers_created += 1);
    }
    device.create_buffer_init(descriptor)
}

/// `device.create_texture_with_data`, counted as an upload.
#[cfg_attr(any(test, feature = "diagnostics"), track_caller)]
pub(crate) fn texture_init(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    descriptor: &wgpu::TextureDescriptor<'_>,
    order: wgpu::util::TextureDataOrder,
    data: &[u8],
) -> wgpu::Texture {
    use wgpu::util::DeviceExt;
    #[cfg(any(test, feature = "diagnostics"))]
    upload(data.len());
    device.create_texture_with_data(queue, descriptor, order, data)
}

/// A moment on the diagnostics clock, which `elapsed_ms` measures from:
/// `Instant` natively and the page's `performance.now()` in the browser,
/// with the feature; nothing without it, where every measure is zero.
#[derive(Clone, Copy)]
pub(crate) struct Moment {
    #[cfg(all(any(test, feature = "diagnostics"), not(target_arch = "wasm32")))]
    at: std::time::Instant,
    #[cfg(all(feature = "diagnostics", target_arch = "wasm32"))]
    at: f64,
}

impl Moment {
    pub fn now() -> Self {
        Self {
            #[cfg(all(any(test, feature = "diagnostics"), not(target_arch = "wasm32")))]
            at: std::time::Instant::now(),
            #[cfg(all(feature = "diagnostics", target_arch = "wasm32"))]
            at: performance_now(),
        }
    }

    /// Milliseconds since this moment.
    pub fn elapsed_ms(self) -> f64 {
        #[cfg(all(any(test, feature = "diagnostics"), not(target_arch = "wasm32")))]
        let elapsed = self.at.elapsed().as_secs_f64() * 1e3;
        #[cfg(all(feature = "diagnostics", target_arch = "wasm32"))]
        let elapsed = performance_now() - self.at;
        #[cfg(not(any(
            all(any(test, feature = "diagnostics"), not(target_arch = "wasm32")),
            all(feature = "diagnostics", target_arch = "wasm32")
        )))]
        let elapsed = {
            let _ = self;
            0.
        };
        elapsed
    }
}

/// The page's or worker's `performance.now()`, in milliseconds; zero where
/// the global has none.
#[cfg(all(feature = "diagnostics", target_arch = "wasm32"))]
fn performance_now() -> f64 {
    use js_sys::wasm_bindgen::{JsCast, JsValue};
    thread_local! {
        static NOW: Option<(JsValue, js_sys::Function)> = {
            let performance = js_sys::Reflect::get(&js_sys::global(), &"performance".into()).ok();
            performance.and_then(|performance| {
                let now = js_sys::Reflect::get(&performance, &"now".into()).ok()?;
                Some((performance, now.dyn_into().ok()?))
            })
        };
    }
    NOW.with(|now| {
        now.as_ref()
            .and_then(|(performance, now)| now.call0(performance).ok()?.as_f64())
            .unwrap_or(0.)
    })
}

/// Runs `work`, one `step` of building, counting its calls and its time.
pub(crate) fn step<T>(step: BuildStep, work: impl FnOnce() -> T) -> T {
    #[cfg(any(test, feature = "diagnostics"))]
    {
        let started = Moment::now();
        let result = work();
        let nanoseconds = (started.elapsed_ms() * 1e6) as u64;
        COUNTERS.with_borrow_mut(|counters| {
            if !counters.steps.iter().any(|time| time.step == step) {
                counters.steps.push(StepTime {
                    step,
                    calls: 0,
                    nanoseconds: 0,
                });
            }
            let time = counters
                .steps
                .iter_mut()
                .find(|time| time.step == step)
                .unwrap();
            time.calls += 1;
            time.nanoseconds += nanoseconds;
        });
        result
    }
    #[cfg(not(any(test, feature = "diagnostics")))]
    {
        let _ = step;
        work()
    }
}

/// Counts a growth of the ray source, which copies it whole.
pub(crate) fn ray_source_growth() {
    #[cfg(any(test, feature = "diagnostics"))]
    COUNTERS.with_borrow_mut(|counters| counters.ray_source_growths += 1);
}

/// Counts a growth of a geometry slab, which copies it whole.
pub(crate) fn geometry_growth() {
    #[cfg(any(test, feature = "diagnostics"))]
    COUNTERS.with_borrow_mut(|counters| counters.geometry_growths += 1);
}

/// Counts a static-edit box recorded, and the `merged` pairs of pending
/// boxes that recording it merged.
pub(crate) fn static_edit(merged: usize) {
    #[cfg(any(test, feature = "diagnostics"))]
    COUNTERS.with_borrow_mut(|counters| {
        counters.static_edit_boxes += 1;
        counters.static_edit_boxes_merged += merged as u64;
    });
    #[cfg(not(any(test, feature = "diagnostics")))]
    let _ = merged;
}

/// This thread's counters so far.
#[cfg(any(test, feature = "diagnostics"))]
pub(crate) fn snapshot() -> Counters {
    COUNTERS.with_borrow(Clone::clone)
}
