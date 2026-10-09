//! What a geometry pass has bound, which both of the executor's forms, a
//! CPU-built list's draws and a GPU-built list's indirect draws, bind
//! through.
use crate::Scene;
use crate::content::identity::{MaterialId, ShaderId};
use crate::view::pipelines::{GeometryPass, GeometryPipelines, Variant};

/// What a geometry pass has bound for its draws so far: the pipeline of
/// each variant of materials without a shader, looked up once, and the
/// bound variant, shader and material, which both executors bind only when
/// they change.
pub(crate) struct Binder<'a> {
    pipelines: &'a GeometryPipelines,
    kind: GeometryPass,
    by_variant: [Option<&'a wgpu::RenderPipeline>; Variant::COUNT],
    pipeline: Option<(Variant, Option<ShaderId>)>,
    material: Option<MaterialId>,
}

impl<'a> Binder<'a> {
    pub fn new(pipelines: &'a GeometryPipelines, kind: GeometryPass) -> Self {
        Self {
            pipelines,
            kind,
            by_variant: [None; Variant::COUNT],
            pipeline: None,
            material: None,
        }
    }

    /// Binds the pipeline of `variant` and `material`'s shader, and
    /// `material`'s group 2, where they differ from the last draw's.
    pub fn bind(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        scene: &Scene,
        variant: Variant,
        material: MaterialId,
    ) {
        // A material's shader is its own, so the same material and variant
        // keep the same pipeline.
        if self.material == Some(material)
            && self.pipeline.is_some_and(|(bound, _)| bound == variant)
        {
            return;
        }
        let drawn = scene.drawn_material(material);
        let shader = drawn.values.shader.map(|shader| shader.shader);
        if self.pipeline != Some((variant, shader)) {
            let pipeline = match shader {
                None => *self.by_variant[variant.index()]
                    .get_or_insert_with(|| self.pipelines.get(self.kind, variant, None)),
                Some(_) => self.pipelines.get(self.kind, variant, shader),
            };
            pass.set_pipeline(pipeline);
            self.pipeline = Some((variant, shader));
        }
        if self.material != Some(material) {
            pass.set_bind_group(2, &drawn.group, &[]);
            self.material = Some(material);
        }
    }

    /// After the caller bound another pipeline: the next `bind` binds its
    /// variant's again.
    pub fn forget_pipeline(&mut self) {
        self.pipeline = None;
    }
}
