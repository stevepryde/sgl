//! Independent f64 GGX equations and frozen pre-change GPU compatibility oracle.
//! See ../../ANISOTROPY.md for authority, tolerances, and diagnostic limitations.
use glam::{DQuat, DVec3};
use std::f64::consts::PI;

// Frozen from git 73c7508:crates/sgl-3d/src/pbr.wgsl. Only function names
// changed so this historical baseline can execute alongside production code.
const HISTORICAL: &str = r#"fn historical_three_fresnel(c:f32,f0:vec3<f32>)->vec3<f32> {
 let f=exp2((-5.55473*c-6.98316)*c);
 return f0*(1.-f)+vec3(f);
}
fn historical_three_specular(n:vec3<f32>,v:vec3<f32>,l:vec3<f32>,r:f32,f0:vec3<f32>)->vec3<f32> {
 let nv=clamp(dot(n,v),0.,1.);let nl=clamp(dot(n,l),0.,1.);
 let h=normalize(v+l);let nh=clamp(dot(n,h),0.,1.);let vh=clamp(dot(v,h),0.,1.);
 let a2=pow(r,4.);let denominator=1.-nh*nh*(1.-a2);
 let distribution=a2/(denominator*denominator*3.14159265359);
 let visibility=0.5/max(nl*sqrt(a2+(1.-a2)*nv*nv)+nv*sqrt(a2+(1.-a2)*nl*nl),0.000001);
 return historical_three_fresnel(vh,f0)*visibility*distribution;
}
fn historical_three_single_scatter(f0:vec3<f32>,dfg:vec2<f32>)->vec3<f32> {
 return f0*dfg.x+vec3(dfg.y);
}
fn historical_direct_three(n:vec3<f32>,v:vec3<f32>,l:vec3<f32>,diffuse:vec3<f32>,f0:vec3<f32>,rough:f32,coat:f32,coat_rough:f32,dfg_v:vec2<f32>,dfg_l:vec2<f32>)->vec3<f32> {
 let nl=clamp(dot(n,l),0.,1.);let nv=clamp(dot(n,v),0.,1.);
 let missing_v=1.-dfg_v.x-dfg_v.y;let missing_l=1.-dfg_l.x-dfg_l.y;
 let average=f0+(vec3(1.)-f0)*.047619;
 let multi=historical_three_single_scatter(f0,dfg_v)*historical_three_single_scatter(f0,dfg_l)*average/
  (vec3(1.)-missing_v*missing_l*average*average+vec3(.000001))*missing_v*missing_l;
 let base=diffuse/3.14159265359+historical_three_specular(n,v,l,rough,f0)+multi;
 if coat<=0. {return base*nl;}
 return base*(1.-coat*historical_three_fresnel(nv,vec3(.04)).x)*nl+coat*historical_three_specular(n,v,l,coat_rough,vec3(.04))*nl;
}
"#;

fn specular_f0(
    n: DVec3,
    v: DVec3,
    l: DVec3,
    t: DVec3,
    rough: f64,
    strength: f64,
    f0: DVec3,
) -> DVec3 {
    let b = n.cross(t).normalize();
    let h = (v + l).normalize();
    let nv = n.dot(v).clamp(0., 1.);
    let nl = n.dot(l).clamp(0., 1.);
    let ab = rough * rough;
    let at = ab + (1. - ab) * strength * strength;
    // Direct mathematical D, unlike the rescaled vector implementation in WGSL.
    let ellipse = (h.dot(t) / at).powi(2) + (h.dot(b) / ab).powi(2) + h.dot(n).powi(2);
    let d = 1. / (PI * at * ab * ellipse * ellipse);
    let projected_v = ((at * t.dot(v)).powi(2) + (ab * b.dot(v)).powi(2) + nv * nv).sqrt();
    let projected_l = ((at * t.dot(l)).powi(2) + (ab * b.dot(l)).powi(2) + nl * nl).sqrt();
    let visibility = 0.5 / (nl * projected_v + nv * projected_l).max(1e-6);
    let vh = v.dot(h).clamp(0., 1.);
    let f = 2f64.powf((-5.55473 * vh - 6.98316) * vh);
    (f0 * (1. - f) + DVec3::splat(f)) * d * visibility
}

fn specular(n: DVec3, v: DVec3, l: DVec3, t: DVec3, rough: f64, strength: f64) -> DVec3 {
    specular_f0(n, v, l, t, rough, strength, DVec3::new(0.54, 0.49, 0.44))
}

fn bent_normal(n: DVec3, v: DVec3, t: DVec3, rough: f64, strength: f64) -> DVec3 {
    let b = n.cross(t).normalize();
    // Orthogonal projection is independent of the shader's double cross form.
    let projected = (v - b * b.dot(v)).normalize();
    let blend = (1. - strength * (1. - rough)).powi(4);
    (projected * (1. - blend) + n * blend).normalize()
}

fn direction(theta: f64, phi: f64) -> DVec3 {
    DVec3::new(
        theta.sin() * phi.cos(),
        theta.sin() * phi.sin(),
        theta.cos(),
    )
}

#[test]
fn anisotropy_gpu_matches_independent_brdf_and_historical_zero() {
    use wgpu::util::DeviceExt;
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let mut cases: Vec<[[f32; 4]; 4]> = Vec::new();
        for rotation in [
            DQuat::IDENTITY,
            DQuat::from_euler(glam::EulerRot::XYZ, 0.81, -1.17, 0.63),
        ] {
            for rough in [0.15, 0.3, 0.7] {
                for strength in [0., 0.00001, 0.6, 1.] {
                    for theta in [0.12, 0.87, 1.565] {
                        for phi in [0., 0.61, 1.57, 2.36] {
                            for axis in [0., 0.43, 1.57, 2.29] {
                                let n = rotation * DVec3::Z;
                                let v = rotation * direction(theta, phi);
                                let l = rotation * direction(theta * 0.83, phi + 2.81);
                                let t = rotation * DVec3::new(f64::cos(axis), f64::sin(axis), 0.);
                                cases.push([
                                    [n.x as f32, n.y as f32, n.z as f32, rough],
                                    [v.x as f32, v.y as f32, v.z as f32, strength],
                                    [
                                        l.x as f32,
                                        l.y as f32,
                                        l.z as f32,
                                        if axis == 0. { 0. } else { 0.65 },
                                    ],
                                    [t.x as f32, t.y as f32, t.z as f32, 0.21],
                                ]);
                            }
                        }
                    }
                }
            }
        }
        // The surface library reads the lit bindings, so the program declares
        // them; this entry point uses only its own two, at free group 0 slots.
        let source = format!(
            "{}\n{}\n{}",
            crate::shading::compose(&[
                &crate::shading::BIND_LIT,
                &crate::shading::SURFACE,
                &crate::shading::SHADOW_MASK_NONE,
            ]),
            HISTORICAL,
            r#"
struct Case { n:vec4<f32>,v:vec4<f32>,l:vec4<f32>,t:vec4<f32> }
@group(0) @binding(3) var<storage,read> cases:array<Case>;
@group(0) @binding(4) var<storage,read_write> result:array<vec4<f32>>;
// The production direct light of a case's surface, with the case's F0,
// diffuse colour and DFG lookups in place of a material's and the table's.
fn observed_direct(c:Case,axis_strength:vec4<f32>)->vec3<f32> {
 var surface:Surface;
 surface.normal=c.n.xyz;
 surface.geometry_normal=c.n.xyz;
 surface.view=c.v.xyz;
 surface.roughness=c.n.w;
 surface.coat=c.l.w;
 surface.coat_roughness=c.t.w;
 surface.anisotropy=axis_strength;
 var reflectance=surface_reflectance(surface,vec2(.8,.025));
 reflectance.diffuse=vec3(.12,.07,.02);
 reflectance.f0=vec3(.54,.49,.44);
 return surface_direct_brdf(surface,reflectance,c.l.xyz,vec2(.73,.04),1.);
}
@compute @workgroup_size(64) fn observe(@builtin(global_invocation_id) id:vec3<u32>) {
 if id.x>=arrayLength(&cases) { return; }
 let c=cases[id.x];let n=c.n.xyz;let v=c.v.xyz;let l=c.l.xyz;
 let f0=vec3(.54,.49,.44);let diffuse=vec3(.12,.07,.02);
 let axis=vec4(c.t.xyz,c.v.w);let zero_axis=vec4(c.t.xyz,0.);
 let dfg_v=vec2(.8,.025);let dfg_l=vec2(.73,.04);
 result[id.x*5u]=vec4(pbr_anisotropic_specular(n,v,l,c.n.w,f0,axis),1.);
 result[id.x*5u+1u]=vec4(observed_direct(c,zero_axis),1.);
 result[id.x*5u+2u]=vec4(historical_direct_three(n,v,l,diffuse,f0,c.n.w,c.l.w,c.t.w,dfg_v,dfg_l),1.);
 result[id.x*5u+3u]=vec4(pbr_anisotropy_bent_normal(n,v,axis,c.n.w),1.);
 result[id.x*5u+4u]=vec4(observed_direct(c,axis),1.);
}
"#
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("anisotropy independent observations"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: None,
            module: &shader,
            entry_point: Some("observe"),
            compilation_options: Default::default(),
            cache: None,
        });
        let input = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&cases),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let size = (cases.len() * 5 * 16) as u64;
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: output.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups((cases.len() as u32).div_ceil(64), 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, size);
        queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        readback.map_async(wgpu::MapMode::Read, .., move |r| tx.send(r).unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        rx.recv().unwrap().unwrap();
        let mapped = readback.get_mapped_range(..);
        let rows: &[[f32; 4]] = bytemuck::cast_slice(&mapped);
        let mut max_relative = 0f64;
        let mut max_bent = 0f64;
        let mut max_historical_f64 = 0f64;
        for (i, c) in cases.iter().enumerate() {
            let vector = |a: [f32; 4]| DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
            // Evaluate the physical equations on the exact uploaded float inputs.
            let (n, v, l, t) = (vector(c[0]), vector(c[1]), vector(c[2]), vector(c[3]));
            let expected = specular(n, v, l, t, c[0][3] as f64, c[1][3] as f64);
            let actual = vector(rows[5 * i]);
            let relative =
                ((actual - expected).abs() / expected.max(DVec3::splat(1e-9))).max_element();
            if c[1][3] > 0. {
                max_relative = max_relative.max(relative);
            } else {
                max_historical_f64 = max_historical_f64.max(relative);
            }
            // 0.05% allows f32 dot/normalization cancellation near narrow lobes;
            // the absolute floor is 2e-6 BRDF units, below measured material signals.
            assert!(
                c[1][3] == 0.
                    || (actual - expected)
                        .abs()
                        .cmple(expected.abs() * 0.0005 + DVec3::splat(2e-6))
                        .all(),
                "case {i}: actual={actual:?}, f64={expected:?}, inputs={c:?}"
            );
            // The coat term is factored separately, so compilers may contract it
            // differently; allow four f32 ulps.
            assert!(
                rows[5 * i + 1]
                    .iter()
                    .zip(rows[5 * i + 2])
                    .all(|(a, b)| (a - b).abs() <= 4. * f32::EPSILON * a.abs().max(b.abs())),
                "historical zero-strength direct result, case {i}: {:?} vs {:?}",
                rows[5 * i + 1],
                rows[5 * i + 2]
            );
            // Full f64 direct composition avoids cancellation when subtracting
            // a narrow historical isotropic GPU peak from a broad anisotropic lobe.
            let f0 = DVec3::new(0.54, 0.49, 0.44);
            let average = f0 + (DVec3::ONE - f0) * 0.047619;
            let missing_v = 1. - 0.8 - 0.025;
            let missing_l = 1. - 0.73 - 0.04;
            let single_v = f0 * 0.8 + DVec3::splat(0.025);
            let single_l = f0 * 0.73 + DVec3::splat(0.04);
            let multi = single_v * single_l * average
                / (DVec3::ONE - missing_v * missing_l * average * average + DVec3::splat(0.000001))
                * missing_v
                * missing_l;
            let base = DVec3::new(0.12, 0.07, 0.02) / PI + expected + multi;
            let nv = n.dot(v).clamp(0., 1.);
            let coat_fresnel = 0.04 + 0.96 * 2f64.powf((-5.55473 * nv - 6.98316) * nv);
            let coat = c[2][3] as f64;
            let coat_spec = specular_f0(n, v, l, t, c[3][3] as f64, 0., DVec3::splat(0.04));
            let expected_direct =
                (base * (1. - coat * coat_fresnel) + coat * coat_spec) * n.dot(l).clamp(0., 1.);
            let actual_direct = vector(rows[5 * i + 4]);
            assert!(
                c[1][3] == 0.
                    || (actual_direct - expected_direct)
                        .abs()
                        .cmple(expected_direct.abs() * 0.0005 + DVec3::splat(2e-6))
                        .all(),
                "direct light case {i}: actual={actual_direct:?}, expected={expected_direct:?}"
            );
            let expected_bent = bent_normal(n, v, t, c[0][3] as f64, c[1][3] as f64);
            let bent_error = vector(rows[5 * i + 3]).distance(expected_bent);
            max_bent = max_bent.max(bent_error);
            assert!(
                bent_error < 2e-5,
                "bent normal case {i}: error={bent_error}"
            );
        }
        eprintln!(
            "{} cases: max nonzero BRDF relative={max_relative:.9e}; historical-vs-f64 relative={max_historical_f64:.9e}; max bent vector distance={max_bent:.9e}; historical zero-strength exact",
            cases.len()
        );
    });
}
