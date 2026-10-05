//! A tracing stage's programs for the paths a frame's rays take (the
//! architecture's Hardware ray tracing, *Bindings and composition*): its
//! shader composed with the portable function set, or with the query
//! module of the hardware form in effect (`shading::ray_trace_root`), each
//! with its pipeline layout and its group 3, the stage's own bindings and,
//! on the hardware path, the scene's TLAS at the one entry
//! `shading::bind::tlas_entry` declares. A path's program is made when a
//! frame first takes it.
use super::cached_group::CachedGroup;
use super::frame::HardwareRays;
use crate::shading::{self, Module, RayQueryForm};

/// One path's program: its shader, its pipeline layout and its group 3.
pub(crate) struct TracePath {
    pub shader: wgpu::ShaderModule,
    pub layout: wgpu::PipelineLayout,
    group: CachedGroup,
}

/// A tracing stage's programs, one for each path a frame took.
pub(crate) struct TracePaths {
    label: &'static str,
    root: &'static Module,
    /// The stage's group 3 bindings.
    entries: Vec<wgpu::BindGroupLayoutEntry>,
    /// Its groups 0 and 1: the lit layout and the scene's.
    layouts: [wgpu::BindGroupLayout; 2],
    paths: Vec<(Option<RayQueryForm>, TracePath)>,
}

impl TracePaths {
    /// The programs of the stage whose shader's root is `root`, which binds
    /// `lit` at group 0, `scene` at group 1 and `entries` at group 3.
    pub fn new(
        label: &'static str,
        root: &'static Module,
        entries: &[wgpu::BindGroupLayoutEntry],
        [lit, scene]: [&wgpu::BindGroupLayout; 2],
    ) -> Self {
        Self {
            label,
            root,
            entries: entries.to_vec(),
            layouts: [lit.clone(), scene.clone()],
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
                let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(self.label),
                    source: wgpu::ShaderSource::Wgsl(
                        shading::compose(&[self.root, shading::ray_trace_root(form)]).into(),
                    ),
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

    /// Group 3 of the path `hardware` takes, binding `entries` and, on the
    /// hardware path, its TLAS.
    pub fn group(
        &mut self,
        device: &wgpu::Device,
        hardware: Option<HardwareRays<'_>>,
        entries: &[(u32, wgpu::BindingResource<'_>)],
    ) -> &wgpu::BindGroup {
        let label = self.label;
        let path = self.path(device, hardware.map(|rays| rays.form));
        match hardware {
            None => path.group.get(device, label, entries),
            Some(rays) => path.group.get(
                device,
                label,
                &[
                    entries,
                    &[(shading::bind::hardware::SCENE_TLAS, rays.tlas.as_binding())],
                ]
                .concat(),
            ),
        }
    }
}
