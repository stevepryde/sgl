//! What the probes' light follows, which a converged volume pauses while it
//! holds still (#163's convergence gate).
use crate::view::frame::FrameContext;

/// What the probes' light follows: the scene's content edits, its deforming
/// instances' while the hardware path traces them (the portable path's rays
/// see no deforming instance), the frame's lights and environment as the
/// frame's data carries them (but for what the camera and the clock
/// change), the environment it binds, the rays a probe may trace, and
/// whether the probe hits' light list takes the scene's lights, as prepare
/// builds it. While they hold still, a converged volume pauses.
#[derive(Clone, Copy)]
pub(super) struct Inputs {
    edits: u64,
    deformation_edits: Option<u64>,
    frame: crate::shading::uniforms::FrameUniform,
    environment: Option<crate::EnvironmentId>,
    max_rays: u32,
    local_lights: bool,
}

/// Which of what the probes' light follows changed since the last frame
/// that ran them (`diagnostics::DynamicGiReport::changes`): any keeps the
/// volume from pausing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DynamicGiChanges {
    /// The volume restarted, or its last frame recorded nothing.
    pub restarted: bool,
    /// An edit to content the rays see or that lights them: an instance's
    /// pose, a light, a material, a model and the like, and while the
    /// hardware path traces them, a deforming instance's deformation.
    pub scene_edits: bool,
    /// The frame's lights, environment, fog or other data the hits take,
    /// but for the camera's cascades and the clock, whose animation phase
    /// counts while an opaque or masked material's surface moves
    /// (`SurfaceMaterial::normal_layers`).
    pub frame: bool,
    pub environment: bool,
    /// The most rays a probe traces (`Settings::dynamic_gi`).
    pub quality: bool,
    /// Whether the probe hits take the scene's lights (a diagnostics layer).
    pub local_lights: bool,
}

impl DynamicGiChanges {
    pub fn any(&self) -> bool {
        self.restarted
            || self.scene_edits
            || self.frame
            || self.environment
            || self.quality
            || self.local_lights
    }
}

impl Inputs {
    pub fn of(ctx: &FrameContext<'_>, max_rays: u32) -> Self {
        let mut frame = ctx.values.frame;
        // The camera's cascades, which probe hits do not use, and the
        // clock, but for the phase of an opaque or masked surface whose
        // record's shading it moves (`Material::record_moves`; rays see no
        // shader); the rays pass through blended ones.
        frame.shadow_cascades = bytemuck::Zeroable::zeroed();
        frame.elapsed_seconds = 0.;
        frame.previous_elapsed_seconds = 0.;
        frame.frame_count = 0;
        if !ctx.scene.materials.holds_moving_records() {
            frame.animation_phase = 0.;
        }
        frame.previous_animation_phase = frame.animation_phase;
        Self {
            edits: ctx.scene.edits,
            deformation_edits: ctx.hardware_rays.map(|_| ctx.scene.deformation_edits),
            frame,
            environment: ctx.input.environment,
            max_rays,
            local_lights: ctx.effective.local_lights,
        }
    }

    /// What differs from `committed`, the last frame's, or a restart where
    /// there is none.
    pub fn changes(&self, committed: Option<&Self>) -> DynamicGiChanges {
        let Some(committed) = committed else {
            return DynamicGiChanges {
                restarted: true,
                ..DynamicGiChanges::default()
            };
        };
        DynamicGiChanges {
            restarted: false,
            scene_edits: self.edits != committed.edits
                || self.deformation_edits != committed.deformation_edits,
            frame: bytemuck::bytes_of(&self.frame) != bytemuck::bytes_of(&committed.frame),
            environment: self.environment != committed.environment,
            quality: self.max_rays != committed.max_rays,
            local_lights: self.local_lights != committed.local_lights,
        }
    }
}
