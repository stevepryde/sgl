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
    pipelines: &'a mut GeometryPipelines,
    kind: GeometryPass,
    by_variant: [Option<wgpu::RenderPipeline>; Variant::COUNT],
    pipeline: Option<(Variant, Option<ShaderId>)>,
    material: Option<MaterialId>,
}

impl<'a> Binder<'a> {
    pub fn new(pipelines: &'a mut GeometryPipelines, kind: GeometryPass) -> Self {
        Self {
            pipelines,
            kind,
            by_variant: std::array::from_fn(|_| None),
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
            match shader {
                None => {
                    let pipeline = self.by_variant[variant.index()].get_or_insert_with(|| {
                        self.pipelines
                            .with_pipeline(scene, self.kind, variant, None, Clone::clone)
                    });
                    pass.set_pipeline(pipeline);
                }
                Some(_) => {
                    self.pipelines
                        .with_pipeline(scene, self.kind, variant, shader, |pipeline| {
                            pass.set_pipeline(pipeline)
                        })
                }
            }
            self.pipeline = Some((variant, shader));
        }
        if self.material != Some(material) {
            pass.set_bind_group(2, &drawn.group, &[]);
            self.material = Some(material);
        }
    }

    /// A paired directional shadow draw keeps the material and uses its own
    /// pipeline. The next ordinary draw must bind its pipeline again.
    pub fn bind_paired(&mut self, pass: &mut wgpu::RenderPass<'_>, scene: &Scene) {
        let (variant, shader) = self
            .pipeline
            .take()
            .expect("paired draw follows its ordinary draw");
        self.pipelines.with_pipeline(
            scene,
            GeometryPass::PairedShadow,
            variant,
            shader,
            |pipeline| pass.set_pipeline(pipeline),
        );
    }
}
