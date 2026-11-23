//! What a geometry pass has bound, which both of the executor's forms, a
//! CPU-built list's draws and a GPU-built list's indirect draws, bind
//! through.
use crate::Scene;
use crate::content::identity::{MaterialId, ShaderId};
use crate::view::pipelines::{GeometryPass, GeometryPipelines, Variant};

/// What a geometry pass has bound for its draws so far: the pipeline of
/// each variant of materials without a shader that sample no map in the
/// shader, looked up once, and the
/// bound variant, shader and material, which both executors bind only when
/// they change.
pub(crate) struct Binder<'a> {
    pipelines: &'a mut GeometryPipelines,
    kind: GeometryPass,
    by_variant: [Option<wgpu::RenderPipeline>; Variant::COUNT],
    pipeline: Option<(Variant, Option<ShaderId>)>,
    /// Whether `pipeline` is bound: not in a planning pass while it is
    /// missing.
    bound: bool,
    material: Option<MaterialId>,
}

impl<'a> Binder<'a> {
    pub fn new(pipelines: &'a mut GeometryPipelines, kind: GeometryPass) -> Self {
        Self {
            pipelines,
            kind,
            by_variant: std::array::from_fn(|_| None),
            pipeline: None,
            bound: false,
            material: None,
        }
    }

    /// Binds the pipeline of `variant` and `material`'s shader, and
    /// `material`'s group 2, where they differ from the last draw's.
    /// Whether the pipeline is bound: not in a planning pass
    /// (`Renderer::compile`) while it is missing, where the caller leaves
    /// the draw's commands out.
    pub fn bind(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        scene: &Scene,
        variant: Variant,
        material: MaterialId,
    ) -> bool {
        // A material's shader is its own, so the same material and variant
        // keep the same pipeline.
        if self.material == Some(material)
            && self.pipeline.is_some_and(|(bound, _)| bound == variant)
        {
            return self.bound;
        }
        let drawn = scene.drawn_material(material);
        let shader = drawn.values.shader.map(|shader| shader.shader);
        if self.pipeline != Some((variant, shader)) {
            self.bound = match (shader, variant.index()) {
                (None, Some(index)) => {
                    let slot = &mut self.by_variant[index];
                    if slot.is_none() {
                        *slot = self.pipelines.with_pipeline(
                            scene,
                            self.kind,
                            variant,
                            None,
                            Clone::clone,
                        );
                    }
                    slot.as_ref()
                        .map(|pipeline| pass.set_pipeline(pipeline))
                        .is_some()
                }
                _ => self
                    .pipelines
                    .with_pipeline(scene, self.kind, variant, shader, |pipeline| {
                        pass.set_pipeline(pipeline)
                    })
                    .is_some(),
            };
            self.pipeline = Some((variant, shader));
        }
        if self.material != Some(material) {
            pass.set_bind_group(2, &drawn.group, &[]);
            self.material = Some(material);
        }
        self.bound
    }

    /// A paired directional shadow draw keeps the material and uses its own
    /// pipeline. The next ordinary draw must bind its pipeline again.
    /// Whether the pipeline is bound, as `bind` reports it.
    pub fn bind_paired(&mut self, pass: &mut wgpu::RenderPass<'_>, scene: &Scene) -> bool {
        let (variant, shader) = self
            .pipeline
            .take()
            .expect("paired draw follows its ordinary draw");
        self.pipelines
            .with_pipeline(
                scene,
                GeometryPass::PairedShadow,
                variant,
                shader,
                |pipeline| pass.set_pipeline(pipeline),
            )
            .is_some()
    }
}
