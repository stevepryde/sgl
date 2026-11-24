//! A tracing stage's programs for the paths a frame's rays take (the
//! architecture's Hardware ray tracing, *Bindings and composition*): its
//! shader composed with the portable function set, or with the query
//! module of the hardware form in effect (`shading::ray_trace_root`), and
//! with the lit provider of the device's binding tier
//! (`shading::lit_provider`), each with its pipeline layout and its group 3, the stage's own bindings and,
//! on the hardware path, the scene's TLAS at the one entry
//! `shading::bind::tlas_entry` declares. A path's program is made when a
//! frame first takes it. The candidate form's programs and pipelines are
//! made inside error scopes: one that fails falls the device back to the
//! baseline form for good (`DeviceRayForm`).
use super::cached_group::CachedGroup;
use super::frame::HardwareRays;
use crate::shading::bind::{BindingTier, LitLayout};
use crate::shading::{self, Module, RayQueryForm};
use crate::view::compile::{Compiled, Compiler, SharedModule};
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, OnceLock};
use wgpu::WasmNotSend;

/// The hardware path's form on the renderer's device (the architecture's
/// Hardware ray tracing, *Candidate form*): its backend's
/// (`RayQueryForm::of_backend`) until a tracing program of the candidate
/// form fails to compile or validate, then the baseline, sticky, since the
/// same programs would fail again.
pub(crate) struct DeviceRayForm {
    backend: RayQueryForm,
    /// Why the candidate form failed, once it did: set by whichever thread
    /// compiled the failing program.
    failure: Arc<OnceLock<String>>,
}

impl DeviceRayForm {
    /// The form a device whose backend runs `backend` takes until it fails.
    pub fn new(backend: RayQueryForm) -> Self {
        Self {
            backend,
            failure: Arc::default(),
        }
    }

    /// The form the device's tracing programs take.
    pub fn form(&self) -> RayQueryForm {
        if self.failure.get().is_some() {
            RayQueryForm::Baseline
        } else {
            self.backend
        }
    }

    /// Why the device fell back from the candidate form, once it did.
    pub fn failure(&self) -> Option<&str> {
        self.failure.get().map(String::as_str)
    }
}

/// One path's program: its shader, its pipeline layout and its group 3.
pub(crate) struct TracePath {
    shader: SharedModule,
    layout: wgpu::PipelineLayout,
    group: CachedGroup,
}

impl TracePath {
    /// The path's module, created here if no pipeline has needed it: for a
    /// diagnostic that composes its own pipeline from it.
    #[cfg(feature = "diagnostics")]
    pub fn shader(&self, device: &wgpu::Device) -> &wgpu::ShaderModule {
        self.shader.get(device)
    }
}

/// A stage's trace pipelines: a slot for each of its own keys and ray
/// query form (`None` the software BVHs).
pub(crate) type TracePipelines<K, P = wgpu::ComputePipeline> =
    HashMap<(K, Option<RayQueryForm>), Compiled<P>>;

/// A tracing stage's programs, one for each path a frame took.
pub(crate) struct TracePaths {
    label: &'static str,
    root: &'static Module,
    /// The device's binding tier, whose lit provider each program composes.
    tier: BindingTier,
    /// The stage's group 3 bindings.
    entries: Vec<wgpu::BindGroupLayoutEntry>,
    /// Its groups 0 and 1: the lit layout and the scene's.
    layouts: [wgpu::BindGroupLayout; 2],
    paths: Vec<(Option<RayQueryForm>, TracePath)>,
}

/// `create`'s result and the first compile, validation or internal error
/// it raised, from error scopes around it, popped at once by polling, as
/// native wgpu reports a scope's errors when it is popped. A scope still
/// pending reports an error too: only native devices trace in hardware.
fn scoped<T>(device: &wgpu::Device, create: impl FnOnce() -> T) -> (T, Option<String>) {
    fn popped(scope: wgpu::ErrorScopeGuard) -> Option<String> {
        let mut error = std::pin::pin!(scope.pop());
        match error
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
        {
            std::task::Poll::Ready(error) => error.map(|error| error.to_string()),
            std::task::Poll::Pending => Some("the error scope did not resolve".into()),
        }
    }
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let created = create();
    let internal = popped(internal);
    let validation = popped(validation);
    (created, validation.or(internal))
}

impl TracePaths {
    /// The programs of the stage whose shader's root is `root`, which binds
    /// `lit` at group 0, `scene` at group 1 and `entries` at group 3, and
    /// composes the lit provider of `lit`'s binding tier.
    pub fn new(
        label: &'static str,
        root: &'static Module,
        entries: &[wgpu::BindGroupLayoutEntry],
        lit: &LitLayout,
        scene: &wgpu::BindGroupLayout,
    ) -> Self {
        Self {
            label,
            root,
            tier: lit.tier,
            entries: entries.to_vec(),
            layouts: [lit.layout.clone(), scene.clone()],
            paths: Vec::new(),
        }
    }

    /// The program of the path `form` takes: the hardware form's, or the
    /// portable path's for none.
    pub fn path(&mut self, device: &wgpu::Device, form: Option<RayQueryForm>) -> &mut TracePath {
        let index = match self.paths.iter().position(|(path, _)| *path == form) {
            Some(index) => index,
            None => {
                let mut entries = self.entries.clone();
                if form.is_some() {
                    entries.push(shading::bind::tlas_entry());
                }
                let group = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some(self.label),
                    entries: &entries,
                });
                let [lit, scene] = &self.layouts;
                let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some(self.label),
                    bind_group_layouts: &[Some(lit), Some(scene), None, Some(&group)],
                    immediate_size: 0,
                });
                let (root, tier) = (self.root, self.tier);
                let shader = SharedModule::new(self.label, move || {
                    shading::compose(&[
                        root,
                        shading::ray_trace_root(form),
                        shading::lit_provider(tier),
                    ])
                });
                self.paths.push((
                    form,
                    TracePath {
                        shader,
                        layout,
                        group: CachedGroup::new(group),
                    },
                ));
                self.paths.len() - 1
            }
        };
        &mut self.paths[index].1
    }

    /// The path the frame's rays take on `hardware`, whose pipeline for
    /// `key` `pipelines` then holds, or none in a planning pass while it is
    /// missing: `create` makes it from the path's module and layout, where
    /// first needed or in a compile job named `label`. The candidate
    /// form's program and pipeline are made inside error scopes; a compile,
    /// validation or internal error falls the device back to the baseline
    /// form for good, which `hardware`'s form then names, and the
    /// baseline's pipeline is made instead. A failure is one naga's or the
    /// driver's compiler (Metal's shader compiler, Vulkan's driver, DX12's
    /// DXC) raises; a wrong result or a driver fault is no failure.
    pub fn pipeline<K: Copy + Eq + Hash, P: WasmNotSend + 'static>(
        &mut self,
        (device, compiler): (&wgpu::Device, &Compiler),
        hardware: Option<HardwareRays<'_>>,
        (pipelines, key): (&mut TracePipelines<K, P>, K),
        label: &'static str,
        create: impl Fn(&wgpu::Device, &wgpu::ShaderModule, &wgpu::PipelineLayout) -> P
        + Clone
        + WasmNotSend
        + 'static,
    ) -> Option<Option<RayQueryForm>> {
        let form = hardware.map(|rays| rays.form());
        if let Some(rays) = hardware
            && form == Some(RayQueryForm::Candidates)
        {
            let path = self.path(device, form);
            let ready = pipelines
                .entry((key, form))
                .or_default()
                .get(compiler, label, || {
                    let (shader, layout) = (path.shader.clone(), path.layout.clone());
                    let (create, failure) = (create.clone(), rays.device_form.failure.clone());
                    move |device: &wgpu::Device| {
                        let (pipeline, error) =
                            scoped(device, || create(device, shader.get(device), &layout));
                        if let Some(error) = error {
                            let _ = failure.set(format!(
                                "a tracing program of the candidate form failed on this device, \
                                 so rays trace in the baseline form: {error}"
                            ));
                        }
                        pipeline
                    }
                })
                .is_some();
            if !ready {
                return None;
            }
            if rays.form() == RayQueryForm::Candidates {
                return Some(form);
            }
            // It failed, here or in its job: the baseline's instead.
            pipelines.retain(|(_, path), _| *path != form);
            self.paths.retain(|(path, _)| *path != form);
        }
        let form = hardware.map(|rays| rays.form());
        let path = self.path(device, form);
        pipelines
            .entry((key, form))
            .or_default()
            .get(compiler, label, || {
                let (shader, layout) = (path.shader.clone(), path.layout.clone());
                move |device: &wgpu::Device| create(device, shader.get(device), &layout)
            })
            .is_some()
            .then_some(form)
    }

    /// Group 3 of the path `hardware` takes, binding `entries` and, on the
    /// hardware path, its TLAS, kept while the TLAS and the entries are.
    pub fn group(
        &mut self,
        device: &wgpu::Device,
        hardware: Option<HardwareRays<'_>>,
        entries: &[(u32, wgpu::BindingResource<'_>)],
    ) -> &wgpu::BindGroup {
        let label = self.label;
        let path = self.path(device, hardware.map(|rays| rays.form()));
        match hardware {
            None => path.group.get(device, label, entries),
            Some(rays) => path.group.get_with_structures(
                device,
                label,
                &[
                    entries,
                    &[(shading::bind::hardware::SCENE_TLAS, rays.tlas.as_binding())],
                ]
                .concat(),
                Some(rays.tlas_generation),
            ),
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::{BindingTier, DeviceRayForm, TracePaths};
    use crate::shading::{Module, RayQueryForm};
    use crate::test_support;
    use crate::view::compile::{Compiled, Compiler};
    use crate::view::frame::HardwareRays;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A program that traces one ray through the scene ray function set.
    static TRACE_ONE: Module = Module {
        name: "trace_paths_test",
        source: "@compute @workgroup_size(1) fn trace_one() {\n _=scene_trace_nearest(SceneRay(vec4(0.),vec4(0.,0.,-1.,1.)),SCENE_SIDES_BOTH);\n}\n",
        deps: &[&crate::shading::SCENE_RAYS_PREDICATE],
    };

    // The candidate form's fallback, which no device here would otherwise
    // take: Metal's candidate programs compile, and no Mac has Vulkan ray
    // queries.
    // Plausible defects: a failed candidate pipeline outside an error scope
    // (its error reaches the device's uncaptured-error handler, which
    // panics in wgpu's default, and the frame binds an invalid pipeline);
    // the fallback not sticky, so each frame tries the failing form again;
    // or the frame's pipeline left on the failed form. The oracle is
    // wgpu's validation, on a device with ray queries, of a candidate
    // pipeline asked for an entry point its program lacks, as a backend's
    // failed compile fails one.
    #[test]
    fn a_failed_candidate_pipeline_falls_back_to_the_baseline_for_good() {
        let Some((device, _queue)) = test_support::ray_tracing_device(|limits| limits) else {
            return;
        };
        let tlas = device.create_tlas(&wgpu::CreateTlasDescriptor {
            label: None,
            max_instances: 1,
            flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
            update_mode: wgpu::AccelerationStructureUpdateMode::Build,
        });
        let device_form = DeviceRayForm::new(RayQueryForm::Candidates);
        let hardware = Some(HardwareRays {
            tlas: &tlas,
            tlas_generation: 0,
            device_form: &device_form,
        });
        let scene = crate::shading::bind::scene(&device);
        let lit = crate::shading::bind::lit(&device, BindingTier::Basic);
        let mut paths = TracePaths::new("trace paths test", &TRACE_ONE, &[], &lit, &scene);
        let compiler = Compiler::new(&device);
        let mut pipelines: HashMap<_, Compiled<wgpu::ComputePipeline>> = HashMap::new();
        // The first pipeline asked for fails; every later one is valid.
        let made = Arc::new(AtomicUsize::new(0));
        let create = {
            let made = made.clone();
            move |device: &wgpu::Device,
                  shader: &wgpu::ShaderModule,
                  layout: &wgpu::PipelineLayout| {
                let first = made.fetch_add(1, Ordering::SeqCst) == 0;
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: None,
                    layout: Some(layout),
                    module: shader,
                    entry_point: Some(if first { "no_such_entry" } else { "trace_one" }),
                    compilation_options: Default::default(),
                    cache: None,
                })
            }
        };
        let gpu = (&device, &compiler);
        let form = paths.pipeline(gpu, hardware, (&mut pipelines, 0), "test", create.clone());
        assert_eq!(form, Some(Some(RayQueryForm::Baseline)));
        assert_eq!(
            made.load(Ordering::SeqCst),
            2,
            "the candidate pipeline, then the baseline's"
        );
        assert!(device_form.failure().is_some());
        // A later pipeline does not try the candidate form again.
        let form = paths.pipeline(gpu, hardware, (&mut pipelines, 1), "test", create);
        assert_eq!(form, Some(Some(RayQueryForm::Baseline)));
        assert_eq!(made.load(Ordering::SeqCst), 3);
        assert_eq!(
            pipelines
                .keys()
                .filter(|(_, form)| *form == Some(RayQueryForm::Candidates))
                .count(),
            0
        );
    }
}
