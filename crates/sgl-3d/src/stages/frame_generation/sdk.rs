//! Standalone AMD SDK sequence from fsr3/component.rs at the workspace pin.
//! Scratch resources are independent of the selected antialiasing/upscaler.
use sp_fidelity::{
    error::FfxErrorCode, frameinterpolation::*, interface::FfxInterfaceRef, opticalflow::*,
    types::*,
};
use sp_fidelity_wgpu::FfxWgpuBackend;
use std::{cell::RefCell, rc::Rc};

pub(super) struct Sdk {
    fi: FfxFrameInterpolationContext,
    optical: FfxOpticalflowContext,
    interface: FfxInterfaceRef,
    shared_context: Option<u32>,
    shared: Vec<FfxResourceInternal>,
    backend: Rc<RefCell<FfxWgpuBackend>>,
    pub output: wgpu::Texture,
    pub transfer: FfxBackbufferTransferFunction,
    pub prepare: FfxFrameInterpolationPrepareDescription,
    display: FfxDimensions2D,
}

impl Sdk {
    pub fn new(
        device: &wgpu::Device,
        backend: Rc<RefCell<FfxWgpuBackend>>,
        interface: FfxInterfaceRef,
        render: [u32; 2],
        display: [u32; 2],
        format: wgpu::TextureFormat,
        encoded: bool,
    ) -> Result<Self, FfxErrorCode> {
        let output = crate::view::targets::target_with_usage(
            device,
            "FSR3 generated image",
            display,
            format,
            wgpu::TextureUsages::STORAGE_BINDING,
        )
        .texture()
        .clone();
        let display = FfxDimensions2D {
            width: display[0],
            height: display[1],
        };
        let mut sdk = Self {
            fi: Default::default(),
            optical: Default::default(),
            interface,
            shared_context: None,
            shared: Vec::new(),
            backend,
            output,
            transfer: if encoded {
                FfxBackbufferTransferFunction::Srgb
            } else {
                FfxBackbufferTransferFunction::Scrgb
            },
            prepare: Default::default(),
            display,
        };
        ffx_opticalflow_context_create(
            &mut sdk.optical,
            &FfxOpticalflowContextDescription {
                backend_interface: sdk.interface.clone(),
                flags: 0,
                resolution: display,
            },
        )?;
        let format = sp_fidelity_wgpu::ffx_get_resource_description_wgpu(&sdk.output).format;
        ffx_frame_interpolation_context_create(
            &mut sdk.fi,
            &FfxFrameInterpolationContextDescription {
                flags: FFX_FRAMEINTERPOLATION_ENABLE_DEPTH_INVERTED
                    | FFX_FRAMEINTERPOLATION_ENABLE_DEPTH_INFINITE
                    | if encoded {
                        0
                    } else {
                        FFX_FRAMEINTERPOLATION_ENABLE_HDR_COLOR_INPUT
                    },
                max_render_size: FfxDimensions2D {
                    width: render[0],
                    height: render[1],
                },
                display_size: display,
                back_buffer_format: format,
                previous_interpolation_source_format: format,
                backend_interface: sdk.interface.clone(),
            },
        )?;
        let optical = ffx_opticalflow_get_shared_resource_descriptions(&sdk.optical)?;
        let fi = ffx_frame_interpolation_get_shared_resource_descriptions(&sdk.fi)?;
        let shared_context = sdk
            .interface
            .borrow_mut()
            .create_backend_context(FfxEffect::SharedResources, None)?;
        sdk.shared_context = Some(shared_context);
        for desc in [
            fi.dilated_depth,
            fi.dilated_motion_vectors,
            fi.reconstructed_prev_nearest_depth,
            optical.optical_flow_vector,
            optical.optical_flow_scd,
        ] {
            sdk.shared.push(
                sdk.interface
                    .borrow_mut()
                    .create_resource(&desc, shared_context)?,
            );
        }
        Ok(sdk)
    }
    fn resources(&self) -> [FfxResource; 5] {
        let mut interface = self.interface.borrow_mut();
        std::array::from_fn(|i| interface.get_resource(self.shared[i]))
    }
    pub fn prepare(
        &mut self,
        list: FfxCommandList,
        mut desc: FfxFrameInterpolationPrepareDescription,
        depth: &wgpu::Texture,
        motion: &wgpu::Texture,
    ) -> Result<(), FfxErrorCode> {
        let [
            dilated_depth,
            dilated_motion_vectors,
            reconstructed_prev_depth,
            _,
            _,
        ] = self.resources();
        desc.command_list = list;
        {
            let mut backend = self.backend.borrow_mut();
            desc.depth = backend.ffx_get_resource_wgpu(
                depth,
                "FSR3 scene depth",
                FFX_RESOURCE_STATE_COMPUTE_READ,
            );
            desc.motion_vectors = backend.ffx_get_resource_wgpu(
                motion,
                "FSR3 scene motion",
                FFX_RESOURCE_STATE_COMPUTE_READ,
            );
        }
        desc.dilated_depth = dilated_depth;
        desc.dilated_motion_vectors = dilated_motion_vectors;
        desc.reconstructed_prev_depth = reconstructed_prev_depth;
        let result = ffx_frame_interpolation_prepare(&mut self.fi, &desc);
        // Only metadata survives this command-list scope. Handles are resolved
        // afresh by optical/interpolate, after the previous list is returned.
        self.prepare = FfxFrameInterpolationPrepareDescription {
            depth: Default::default(),
            motion_vectors: Default::default(),
            dilated_depth: Default::default(),
            dilated_motion_vectors: Default::default(),
            reconstructed_prev_depth: Default::default(),
            command_list: 0,
            ..desc
        };
        result
    }
    pub fn optical(
        &mut self,
        list: FfxCommandList,
        color: &wgpu::Texture,
        reset: bool,
    ) -> Result<(), FfxErrorCode> {
        let [_, _, _, optical_flow_vector, optical_flow_scd] = self.resources();
        let color = self.backend.borrow_mut().ffx_get_resource_wgpu(
            color,
            "FSR3 display color",
            FFX_RESOURCE_STATE_COMPUTE_READ,
        );
        ffx_opticalflow_context_dispatch(
            &mut self.optical,
            &FfxOpticalflowDispatchDescription {
                command_list: list,
                color,
                optical_flow_vector,
                optical_flow_scd,
                reset,
                backbuffer_transfer_function: self.transfer as i32,
                min_max_luminance: FfxFloatCoords2D { x: 0., y: 1000. },
            },
        )
    }
    pub fn interpolate(
        &mut self,
        list: FfxCommandList,
        color: &wgpu::Texture,
        reset: bool,
    ) -> Result<(), FfxErrorCode> {
        let [
            dilated_depth,
            dilated_motion_vectors,
            reconstructed_prev_depth,
            optical_flow_vector,
            optical_flow_scene_change_detection,
        ] = self.resources();
        let (color, output) = {
            let mut backend = self.backend.borrow_mut();
            (
                backend.ffx_get_resource_wgpu(
                    color,
                    "FSR3 display color",
                    FFX_RESOURCE_STATE_COMPUTE_READ,
                ),
                backend.ffx_get_resource_wgpu(
                    &self.output,
                    "FSR3 generated image",
                    FFX_RESOURCE_STATE_UNORDERED_ACCESS,
                ),
            )
        };
        let p = &self.prepare;
        ffx_frame_interpolation_dispatch(
            &mut self.fi,
            &FfxFrameInterpolationDispatchDescription {
                command_list: list,
                display_size: self.display,
                render_size: p.render_size,
                current_back_buffer: color,
                output,
                optical_flow_vector,
                optical_flow_scene_change_detection,
                optical_flow_block_size: 8,
                optical_flow_scale: FfxFloatCoords2D {
                    x: 1. / self.display.width as f32,
                    y: 1. / self.display.height as f32,
                },
                frame_time_delta: p.frame_time_delta,
                camera_near: p.camera_near,
                camera_far: p.camera_far,
                camera_fov_angle_vertical: p.camera_fov_angle_vertical,
                view_space_to_meters_factor: p.view_space_to_meters_factor,
                reset,
                frame_id: p.frame_id,
                interpolation_rect: FfxRect2D {
                    left: 0,
                    top: 0,
                    width: self.display.width as i32,
                    height: self.display.height as i32,
                },
                dilated_depth,
                dilated_motion_vectors,
                reconstructed_prev_depth,
                back_buffer_transfer_function: self.transfer,
                min_max_luminance: [0., 1000.],
                ..Default::default()
            },
        )
    }
}
impl Drop for Sdk {
    fn drop(&mut self) {
        let _ = ffx_frame_interpolation_context_destroy(&mut self.fi);
        let _ = ffx_opticalflow_context_destroy(&mut self.optical);
        if let Some(id) = self.shared_context {
            let mut interface = self.interface.borrow_mut();
            for resource in self.shared.drain(..) {
                let _ = interface.destroy_resource(resource, id);
            }
            let _ = interface.destroy_backend_context(id);
        }
    }
}
