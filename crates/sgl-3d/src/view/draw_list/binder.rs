//! What a geometry pass has bound, which both of the executor's forms, a
//! CPU-built list's draws and a GPU-built list's indirect draws, bind
//! through.
use crate::Scene;
use crate::content::identity::MaterialId;
use crate::view::pipelines::{GeometryPass, GeometryPipelines, Variant};

/// What a geometry pass has bound for its draws so far: the pipeline of
/// each variant, looked up once, and the bound variant and material, which
/// both executors bind only when they change.
pub(crate) struct Binder<'a> {
    pipelines: &'a GeometryPipelines,
    kind: GeometryPass,
    by_variant: [Option<&'a wgpu::RenderPipeline>; Variant::COUNT],
    variant: Option<Variant>,
    material: Option<MaterialId>,
}

impl<'a> Binder<'a> {
    pub fn new(pipelines: &'a GeometryPipelines, kind: GeometryPass) -> Self {
        Self {
            pipelines,
            kind,
            by_variant: [None; Variant::COUNT],
            variant: None,
            material: None,
        }
    }

    /// Binds `variant`'s pipeline and `material`'s group 2 where they
    /// differ from the last draw's.
    pub fn bind(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        scene: &Scene,
        variant: Variant,
        material: MaterialId,
    ) {
        if self.variant != Some(variant) {
            let pipeline = *self.by_variant[variant.index()]
                .get_or_insert_with(|| self.pipelines.get(self.kind, variant));
            pass.set_pipeline(pipeline);
            self.variant = Some(variant);
        }
        if self.material != Some(material) {
            pass.set_bind_group(2, &scene.drawn_material(material).group, &[]);
            self.material = Some(material);
        }
    }
}
