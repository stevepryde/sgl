//! Analytic screen-space fixtures for the opaque stage's passes: depth,
//! G-buffer normals and lit colour that a full-screen triangle writes from
//! primary-ray/plane intersections, with no reconstruction, horizon search,
//! noise or filtering of the passes they test.
use std::ops::Range;

/// A wall at x = 0.6 m standing on the plane z = -3 m, which faces a camera
/// at the origin looking down -z.
#[derive(Clone, Copy)]
pub(crate) enum Wall {
    /// Facing the plane (normal -x), with this lit colour.
    Facing([f32; 3]),
    /// The sky where the wall would be, with this colour.
    Sky([f32; 3]),
}

/// The plane z = -3 m facing the camera (normal +z), with an optional wall
/// (`Wall`) and an optional plate 1 m from the camera over the rows
/// `plate`, facing the camera, or the plane where `plate_faces_plane`;
/// `rotated` turns the camera and scene together by 180° about y, which
/// keeps what is projected and turns the world normals.
#[derive(Clone)]
pub(crate) struct PlaneAndWall {
    pub wall: Option<Wall>,
    pub rotated: bool,
    pub plate: Option<Range<u32>>,
    pub plate_faces_plane: bool,
    pub plane_color: [f32; 3],
    pub plate_color: [f32; 3],
}

impl PlaneAndWall {
    /// The plane alone, unlit.
    pub fn plane() -> Self {
        Self {
            wall: None,
            rotated: false,
            plate: None,
            plate_faces_plane: false,
            plane_color: [0.; 3],
            plate_color: [0.; 3],
        }
    }
}

/// Independent cosine-weighted hemisphere ray integral. The receiver is on
/// z=-3 with normal +Z; the only occluder is the wall x=0.6, z>=-3.
pub(crate) fn wall_visibility(distance: f64, radius: f64) -> f64 {
    let mut visible = 0usize;
    for radial in 0..256 {
        let r = ((radial as f64 + 0.5) / 256.0).sqrt();
        for angular in 0..256 {
            let phi = (angular as f64 + 0.5) / 256.0 * std::f64::consts::TAU;
            let ray_x = r * phi.cos();
            let hit_distance = distance / ray_x;
            if ray_x <= 0.0 || hit_distance > radius {
                visible += 1;
            }
        }
    }
    visible as f64 / (256 * 256) as f64
}

fn vector([x, y, z]: [f32; 3]) -> String {
    format!("vec3({x:?},{y:?},{z:?})")
}

/// The depth, normals and lit colour of `fixture` at `size` under
/// `projection`.
pub(crate) fn plane_and_wall(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: [u32; 2],
    projection: glam::Mat4,
    fixture: &PlaneAndWall,
) -> [wgpu::TextureView; 3] {
    let (wall, sky, wall_color) = match fixture.wall {
        None => (false, false, [0.; 3]),
        Some(Wall::Facing(color)) => (true, false, color),
        Some(Wall::Sky(color)) => (false, true, color),
    };
    let fragment = format!(
        r#"
 let uv=pixel.xy/vec2({width}.0,{height}.0);
 let ray=vec3((uv.x*2.-1.)/{px},(1.-uv.y*2.)/{py},-1.);
 var t=3.; var n=vec3(0.,0.,1.); var c={plane};
 var sky=false;
 if ({wall} || {sky}) && ray.x>0. {{ let wt=0.6/ray.x; if wt<t {{ t=wt; n=vec3(-1.,0.,0.); c={wall_color}; sky={sky}; }} }}
 if pixel.y>={plate_top}. && pixel.y<{plate_bottom}. {{ t=1.; n=vec3(0.,0.,{plate_normal}); c={plate}; sky=false; }}
 let z=ray.z*t;
 if {rotated} {{ n=vec3(-n.x,n.y,-n.z); }}
 var o:Output; o.normal=gbuffer_encode_normals(n,n); o.color=vec4(c,1.); o.depth=select(({pz}*z+{pw})/(-z),0.,sky); return o;
"#,
        width = size[0],
        height = size[1],
        px = projection.x_axis.x,
        py = projection.y_axis.y,
        pz = projection.z_axis.z,
        pw = projection.w_axis.z,
        rotated = fixture.rotated,
        plane = vector(fixture.plane_color),
        wall_color = vector(wall_color),
        plate = vector(fixture.plate_color),
        plate_normal = if fixture.plate_faces_plane {
            "-1."
        } else {
            "1."
        },
        plate_top = fixture.plate.as_ref().map_or(0, |rows| rows.start),
        plate_bottom = fixture.plate.as_ref().map_or(0, |rows| rows.end),
    );
    rasterize(device, queue, size, &fragment)
}

/// The depth, normal and colour targets a full-screen triangle writes,
/// `fragment` being the body of a fragment shader that takes the pixel's
/// position and returns an `Output` of its encoded normals, colour and
/// device depth; a colour it leaves unset is 0.
pub(crate) fn rasterize(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: [u32; 2],
    fragment: &str,
) -> [wgpu::TextureView; 3] {
    let source = format!(
        r#"
@vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32> {{
 let v=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.)); return vec4(v[i],0.,1.);
}}
struct Output {{ @location(0) normal:vec4<f32>, @location(1) color:vec4<f32>, @builtin(frag_depth) depth:f32 }}
@fragment fn fs(@builtin(position) pixel:vec4<f32>)->Output {{{fragment}}}
"#
    );
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("analytic opaque fixture"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                "{}\n{source}",
                crate::shading::compose(&[&crate::shading::GBUFFER])
            )
            .into(),
        ),
    });
    let color = crate::shading::gbuffer::COLOR;
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("analytic opaque fixture"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[
                Some(wgpu::TextureFormat::Rgba16Float.into()),
                Some(color.into()),
            ],
        }),
        multiview_mask: None,
        cache: None,
    });
    let texture = |format| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("analytic opaque fixture"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
            .create_view(&Default::default())
    };
    let depth = texture(wgpu::TextureFormat::Depth32Float);
    let normals = texture(wgpu::TextureFormat::Rgba16Float);
    let colors = texture(color);
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let attachment = |view| {
            Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("analytic opaque fixture"),
            color_attachments: &[attachment(&normals), attachment(&colors)],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.draw(0..3, 0..1);
    }
    queue.submit([encoder.finish()]);
    [depth, normals, colors]
}
