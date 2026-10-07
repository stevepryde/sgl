//! The irradiance volume observed through what every view shades with: its
//! sample (`irradiance_volume_light`), the one determination
//! (`surface_indirect_diffuse`), lit shading (`shade_lit`) and real frames.
//! A uniform field reproduces Valve's ambient cube for any normal; a region
//! write lands at its cells and nowhere else; a voxel face reads the cell
//! before it; a scroll keeps the cells that stay where they were in the
//! world and a render origin move keeps the field where it was; the share
//! fades over the cell past each face; the volume lights receivers below
//! charts and above the dynamic GI volume and ambient cubes, takes the
//! frame's ambient by its sky visibility in place of the environment and
//! the fill, and occludes the sky's specular but not a probe's, in lit
//! shading and in completion; and the scene refuses placements and regions
//! it cannot hold, changing nothing.
use super::PreparedIrradianceRegion;
use crate::renderer::Renderer;
use crate::settings::{self, DynamicGiQuality, Settings};
use crate::shading::gbuffer;
use crate::static_lighting::AmbientCube;
use crate::{
    Backdrop, Camera, DynamicGiVolume, EnvironmentId, FrameInput, HemisphereLight, InstanceState,
    IrradianceCell, IrradianceVolume, Mobility, Scene, SceneError, test_support,
};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [16, 16];

type Gpu<'a> = (&'a wgpu::Device, &'a wgpu::Queue);

fn settings() -> Settings {
    Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        ambient_occlusion: settings::AmbientOcclusionQuality::Off,
        screen_space_reflections: settings::ScreenSpaceReflections::Off,
        world_space_reflections: settings::WorldSpaceReflections::Off,
        dynamic_gi: DynamicGiQuality::Off,
        ..Settings::default()
    }
}

/// A camera at the origin looking down -Z, with no light, fill or
/// environment.
fn input() -> FrameInput {
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    input.backdrop = Backdrop::Color([0.; 3]);
    input.hemisphere_light = HemisphereLight {
        sky_color: [0.; 3],
        ground_color: [0.; 3],
        intensity: 0.,
    };
    input
}

/// An environment of uniform radiance `radiance`.
fn uniform_environment((device, queue): Gpu, scene: &mut Scene, radiance: f32) -> EnvironmentId {
    let texel: Vec<u8> = [radiance, radiance, radiance, 1.]
        .iter()
        .flat_map(|&value| test_support::to_half(value).to_le_bytes())
        .collect();
    scene
        .add_environment(device, queue, &test_support::environment([255; 4], &texel))
        .unwrap()
}

/// A cell of grey `light` toward every face, the sky `visible` from each.
fn cell(light: f32, visible: f32) -> IrradianceCell {
    IrradianceCell {
        irradiance: AmbientCube {
            irradiance: [[light; 3]; 6],
        },
        sky_visibility: [visible; 6],
    }
}

/// Writes every cell of the installed `volume` as `value` gives it by its
/// index, x fastest, then y, then z.
fn fill(
    queue: &wgpu::Queue,
    scene: &mut Scene,
    volume: IrradianceVolume,
    value: impl Fn([u32; 3]) -> IrradianceCell,
) {
    let [nx, ny, nz] = volume.cells;
    let values: Vec<_> = (0..nz)
        .flat_map(|z| (0..ny).flat_map(move |y| (0..nx).map(move |x| [x, y, z])))
        .map(value)
        .collect();
    let region = PreparedIrradianceRegion::new(volume.origin, volume.cells, &values).unwrap();
    scene.write_irradiance_cells(queue, &region).unwrap();
}

/// Where a floor facing +Y at the bottom of cell `index` of `volume` lies:
/// half a cell along its normal is the cell's centre.
fn floor_of(volume: IrradianceVolume, index: [u32; 3]) -> Vec3 {
    let [x, y, z] = index.map(|i| i as f32);
    volume.origin + volume.cell_size * Vec3::new(x + 0.5, y, z + 0.5)
}

fn vec3s(points: impl IntoIterator<Item = Vec3>) -> String {
    points
        .into_iter()
        .map(|p| format!("vec3({:?},{:?},{:?})", p.x, p.y, p.z))
        .collect::<Vec<_>>()
        .join(",")
}

/// Shading fixtures for observations.
const FIXTURES: &str = r#"
// A rough grey dielectric, or a smooth white metal, at `position` facing
// `normal` and seen along it: static, with no chart, and with an ambient
// cube of 0.25 on every face that it takes when made moving.
fn fixture_surface(position:vec3<f32>,normal:vec3<f32>,metal:bool,environment_scale:f32)->Surface {
 var s:Surface;
 s.position=position;
 s.view=normal;
 s.normal=normal;
 s.geometry_normal=normal;
 s.coat_normal=normal;
 s.base=select(vec4(.5,.5,.5,1.),vec4(1.),metal);
 s.metallic=select(0.,1.,metal);
 s.dielectric_f0=vec3(.04);
 s.specular=1.;
 s.roughness=select(1.,.1,metal);
 s.environment_scale=environment_scale;
 s.occlusion=1.;
 s.front=true;
 for (var face=0u;face<6u;face++) {
  s.baked_irradiance[face]=vec4(.25);
 }
 return s;
}
fn fixture_shaded(s:Surface,receiver:u32,environment_specular:bool)->Shaded {
 let context=ShadeContext(vec2(0.),receiver,environment_specular,true,cluster_range(s.position,vec2(0.)),untraced_reflection());
 return shade_lit(s,context);
}
"#;

/// Runs `statements`, which write `output`, after `FIXTURES` in compute
/// under the frame's lit group 0, and reads back its `count` outputs.
fn observe(
    gpu: Gpu,
    (renderer, scene): (&mut Renderer, &mut Scene),
    input: &FrameInput,
    settings: &Settings,
    statements: &str,
    count: usize,
) -> Vec<[f32; 4]> {
    let observation = format!(
        r#"{FIXTURES}
@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{
{statements}
}}
"#
    );
    test_support::observe_ray_hits(
        gpu.0,
        gpu.1,
        renderer,
        scene,
        input,
        settings,
        &observation,
        count,
    )
}

/// `irradiance_volume_light` at each (position, geometry normal, shading
/// normal): the red of its own light, its sky visibility and its share.
fn field(
    gpu: Gpu,
    (renderer, scene): (&mut Renderer, &mut Scene),
    input: &FrameInput,
    queries: &[(Vec3, Vec3, Vec3)],
) -> Vec<[f32; 3]> {
    let count = queries.len();
    let statements = format!(
        r#" let positions=array<vec3<f32>,{count}>({});
 let geometry=array<vec3<f32>,{count}>({});
 let normals=array<vec3<f32>,{count}>({});
 for (var i=0u;i<{count}u;i++) {{
  let light=irradiance_volume_light(positions[i],geometry[i],normals[i]);
  output[i]=vec4(light.light.r,light.sky_visibility,light.share,0.);
 }}"#,
        vec3s(queries.iter().map(|q| q.0)),
        vec3s(queries.iter().map(|q| q.1)),
        vec3s(queries.iter().map(|q| q.2)),
    );
    observe(
        gpu,
        (renderer, scene),
        input,
        &settings(),
        &statements,
        count,
    )
    .into_iter()
    .map(|[light, visible, share, _]| [light, visible, share])
    .collect()
}

/// Floors facing +Y at the bottom of each of `cells` of `volume`: the
/// cell's own light and sky visibility toward +Y, and the volume's share.
fn floors(
    gpu: Gpu,
    (renderer, scene): (&mut Renderer, &mut Scene),
    volume: IrradianceVolume,
    cells: &[[u32; 3]],
) -> Vec<[f32; 3]> {
    let queries: Vec<_> = cells
        .iter()
        .map(|&index| (floor_of(volume, index), Vec3::Y, Vec3::Y))
        .collect();
    field(gpu, (renderer, scene), &input(), &queries)
}

/// A cell whose face `f`, in the cube's order, holds light `base + f / 8`,
/// the sky half visible from each, so each face of every cell is told apart
/// (exactly, in RGBA16F, below a base of 128).
fn faced(base: f32) -> IrradianceCell {
    IrradianceCell {
        irradiance: AmbientCube {
            irradiance: std::array::from_fn(|face| [base + face as f32 / 8.; 3]),
        },
        sky_visibility: [0.5; 6],
    }
}

/// The light of each face, in the cube's order, of each of `cells` of
/// `volume`, read through the shading normals along the six axes at the
/// floor of the cell.
fn faces(
    gpu: Gpu,
    (renderer, scene): (&mut Renderer, &mut Scene),
    volume: IrradianceVolume,
    cells: &[[u32; 3]],
) -> Vec<[f32; 6]> {
    let normals = [
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::Z,
        Vec3::NEG_Z,
    ];
    let queries: Vec<_> = cells
        .iter()
        .flat_map(|&index| normals.map(|normal| (floor_of(volume, index), Vec3::Y, normal)))
        .collect();
    field(gpu, (renderer, scene), &input(), &queries)
        .chunks_exact(6)
        .map(|cell| std::array::from_fn(|face| cell[face][0]))
        .collect()
}

fn every_cell(cells: [u32; 3]) -> Vec<[u32; 3]> {
    let [nx, ny, nz] = cells;
    (0..nz)
        .flat_map(|z| (0..ny).flat_map(move |y| (0..nx).map(move |x| [x, y, z])))
        .collect()
}

fn close(actual: f32, expected: f32, tolerance: f32) -> bool {
    (actual - expected).abs() <= tolerance
}

fn fixture(gpu: Gpu) -> (Renderer, Scene) {
    let (device, queue) = gpu;
    (
        Renderer::for_test(device, queue, SIZE, &settings()),
        Scene::new(device, queue),
    )
}

// Plausible defects: the faces land in the wrong slab (the positive and
// negative halves swapped, the X, Y and Z slabs in another order), the
// cube's face order is mapped wrongly, the visibility is stored or read as
// itself rather than as occlusion, or the blend weighs the faces other than
// by the normal's squared components. The oracle is Valve's ambient cube
// (Mitchell 2006, slide 28, as Bevy samples it): in a field whose every
// cell holds faces of light 1, 2, 4, 8, 16 and 32 and sky visibility 1,
// 0.75, 0.5, 0.25, 0 and 0.125 (+X, -X, +Y, -Y, +Z, -Z), a normal along an
// axis reads that face, (1, 2, -2) / 3 reads +X, +Y and -Z by 1/9, 4/9 and
// 4/9, and (-3, 0, 4) / 5 reads -X and +Z by 9/25 and 16/25.
#[test]
fn a_uniform_field_blends_the_faces_a_normal_points_to_by_its_squared_components() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (mut renderer, mut scene) = fixture((&device, &queue));
    let volume = IrradianceVolume {
        origin: Vec3::new(-2., -1., -3.),
        cell_size: Vec3::ONE,
        cells: [4, 3, 6],
    };
    scene
        .set_irradiance_volume(&device, &queue, Some(volume))
        .unwrap();
    let light = [1., 2., 4., 8., 16., 32.];
    let visible = [1., 0.75, 0.5, 0.25, 0., 0.125];
    fill(&queue, &mut scene, volume, |_| IrradianceCell {
        irradiance: AmbientCube {
            irradiance: light.map(|value| [value; 3]),
        },
        sky_visibility: visible,
    });
    let normals = [
        (Vec3::X, 1., 1.),
        (Vec3::NEG_X, 2., 0.75),
        (Vec3::Y, 4., 0.5),
        (Vec3::NEG_Y, 8., 0.25),
        (Vec3::Z, 16., 0.),
        (Vec3::NEG_Z, 32., 0.125),
        (
            Vec3::new(1., 2., -2.) / 3.,
            (1. + 16. + 128.) / 9.,
            (1. + 2. + 0.5) / 9.,
        ),
        (Vec3::new(-3., 0., 4.) / 5., (18. + 256.) / 25., 6.75 / 25.),
    ];
    let points = [Vec3::new(0.3, 0.2, -0.4), Vec3::new(-1.6, 1.2, 2.2)];
    let queries: Vec<_> = points
        .iter()
        .flat_map(|&point| normals.iter().map(move |&(n, _, _)| (point, Vec3::Y, n)))
        .collect();
    let answers = field(
        (&device, &queue),
        (&mut renderer, &mut scene),
        &input(),
        &queries,
    );
    let expected = points.iter().flat_map(|_| normals.iter());
    for ((query, answer), (_, light, visible)) in queries.iter().zip(&answers).zip(expected) {
        assert!(
            close(answer[0], *light, light * 1e-3)
                && close(answer[1], *visible, 1e-3)
                && answer[2] == 1.,
            "{query:?}: {answer:?}, expected light {light} and visibility {visible}"
        );
    }
}

// Plausible defects: a region lands at the wrong cells (its corner
// misplaced on the lattice, its slab offsets wrong for an interior box),
// its values are packed in another order than x, then y, then z, or it
// spills over neighbouring cells. The oracle is the API's: over a volume
// written whole with light 1 and visibility 1, a box of 2 by 1 by 3 cells
// written with light 5 + i at its i-th cell and visibility 0.5 reads those
// values at its cells, by world position, and every other cell keeps its
// own.
#[test]
fn a_region_write_changes_its_cells_and_no_others() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (mut renderer, mut scene) = fixture((&device, &queue));
    let volume = IrradianceVolume {
        origin: Vec3::new(10., -2., 10.),
        cell_size: Vec3::ONE,
        cells: [6, 4, 6],
    };
    scene
        .set_irradiance_volume(&device, &queue, Some(volume))
        .unwrap();
    fill(&queue, &mut scene, volume, |_| cell(1., 1.));
    let first = [1, 1, 2];
    let size = [2, 1, 3];
    let values: Vec<_> = (0..6).map(|i| cell(5. + i as f32, 0.5)).collect();
    let corner = volume.origin + Vec3::from_array(first.map(|i| i as f32));
    let region = PreparedIrradianceRegion::new(corner, size, &values).unwrap();
    scene.write_irradiance_cells(&queue, &region).unwrap();
    let cells = every_cell(volume.cells);
    let answers = floors(
        (&device, &queue),
        (&mut renderer, &mut scene),
        volume,
        &cells,
    );
    for (index, answer) in cells.iter().zip(&answers) {
        let within: Option<Vec<u32>> = (0..3)
            .map(|axis| {
                index[axis]
                    .checked_sub(first[axis])
                    .filter(|&d| d < size[axis])
            })
            .collect();
        let expected = match within {
            Some(d) => [5. + (d[0] + 2 * (d[1] + d[2])) as f32, 0.5, 1.],
            None => [1., 1., 1.],
        };
        assert_eq!(*answer, expected, "cell {index:?}");
    }
}

// Plausible defects: no offset, an offset along the shading normal, or one
// of another length. The oracle is the voxel face: of two cells stacked, a
// dark solid one below (light 0, visibility 0) and an air one above (light
// 3, visibility 1), a floor on the face between them reads the air cell
// whole whatever its shading normal, and a ceiling there, facing down, the
// solid cell; sampled at the face itself, the filter would blend them
// half and half.
#[test]
fn a_face_reads_the_cell_before_it_along_its_geometry_normal() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (mut renderer, mut scene) = fixture((&device, &queue));
    let volume = IrradianceVolume {
        origin: Vec3::ZERO,
        cell_size: Vec3::ONE,
        cells: [1, 2, 1],
    };
    scene
        .set_irradiance_volume(&device, &queue, Some(volume))
        .unwrap();
    fill(&queue, &mut scene, volume, |[_, y, _]| {
        if y == 0 { cell(0., 0.) } else { cell(3., 1.) }
    });
    let face = Vec3::new(0.5, 1., 0.5);
    let answers = field(
        (&device, &queue),
        (&mut renderer, &mut scene),
        &input(),
        &[
            (face, Vec3::Y, Vec3::Y),
            (face, Vec3::Y, Vec3::new(0.6, 0.8, 0.)),
            (face, Vec3::NEG_Y, Vec3::NEG_Y),
        ],
    );
    assert_eq!(answers[0], [3., 1., 1.], "the floor");
    assert!(
        close(answers[1][0], 3., 1e-5) && close(answers[1][1], 1., 1e-5),
        "a normal-mapped floor: {:?}",
        answers[1]
    );
    assert_eq!(answers[2], [0., 0., 1.], "the ceiling");
}

/// A volume of 4 by 3 by 4 cells of 2 metres, each holding light 1 + its
/// index in the world lattice (x + 4 y + 12 z) and visibility 0.5, each
/// face's light an eighth more than the one before it (`faced`).
const WORLD: IrradianceVolume = IrradianceVolume {
    origin: Vec3::new(-4., 0., -4.),
    cell_size: Vec3::splat(2.),
    cells: [4, 3, 4],
};

fn world_light([x, y, z]: [i64; 3]) -> f32 {
    (1 + x + 4 * y + 12 * z) as f32
}

fn add_world(gpu: Gpu, scene: &mut Scene) {
    let (device, queue) = gpu;
    scene
        .set_irradiance_volume(device, queue, Some(WORLD))
        .unwrap();
    fill(queue, scene, WORLD, |index| {
        faced(world_light(index.map(i64::from)))
    });
}

// Plausible defects: a scroll that moves the cells the wrong way or by the
// wrong amount, loses the cells that stay, keeps stale cells where new
// ones entered, or does not snap its origin to the lattice; and a residual
// off the lattice, or another count, taken as a scroll. The oracle is the
// world: installed again 1 cell along +x and 2 along -z (with a residual of
// a hundred-thousandth of a cell), the volume's origin moves by exactly
// those cells, each cell that stays reads the light written at its world
// position on each of its six faces, and each cell that entered the
// fallback, light 0 and visibility 1; installed 0.3 cells off its lattice,
// or with another count, every cell is the fallback.
#[test]
fn a_scroll_keeps_the_cells_that_stay_where_they_were_in_the_world() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (mut renderer, mut scene) = fixture((&device, &queue));
    add_world((&device, &queue), &mut scene);
    let shift = [1i64, 0, -2];
    let moved = WORLD.origin + WORLD.cell_size * Vec3::new(1., 0., -2.);
    scene
        .set_irradiance_volume(
            &device,
            &queue,
            Some(IrradianceVolume {
                origin: moved + Vec3::splat(2e-5),
                ..WORLD
            }),
        )
        .unwrap();
    let placed = scene.irradiance_volume().unwrap();
    assert!(
        (placed.origin - moved).abs().max_element() < 1e-6,
        "{placed:?}"
    );
    let cells = every_cell(WORLD.cells);
    let lights = faces(
        (&device, &queue),
        (&mut renderer, &mut scene),
        placed,
        &cells,
    );
    let answers = floors(
        (&device, &queue),
        (&mut renderer, &mut scene),
        placed,
        &cells,
    );
    for ((index, light), answer) in cells.iter().zip(&lights).zip(&answers) {
        let world: [i64; 3] = std::array::from_fn(|axis| i64::from(index[axis]) + shift[axis]);
        let stayed = (0..3).all(|axis| (0..i64::from(WORLD.cells[axis])).contains(&world[axis]));
        let (expected, visible) = if stayed {
            let base = world_light(world);
            (std::array::from_fn(|face| base + face as f32 / 8.), 0.5)
        } else {
            ([0.; 6], 1.)
        };
        assert_eq!(*light, expected, "cell {index:?}, world cell {world:?}");
        assert_eq!(answer[1], visible, "cell {index:?}, world cell {world:?}");
    }
    for volume in [
        IrradianceVolume {
            origin: placed.origin + Vec3::new(0.6, 0., 0.),
            ..placed
        },
        IrradianceVolume {
            cells: [4, 3, 5],
            ..placed
        },
    ] {
        add_world((&device, &queue), &mut scene);
        scene
            .set_irradiance_volume(&device, &queue, Some(volume))
            .unwrap();
        assert_eq!(scene.irradiance_volume(), Some(volume));
        let answers = floors(
            (&device, &queue),
            (&mut renderer, &mut scene),
            volume,
            &cells,
        );
        assert!(
            answers.iter().all(|answer| *answer == [0., 1., 1.]),
            "another placement kept cells: {volume:?}"
        );
    }
}

// Plausible defects: a scroll longer than the stripe it moves cells
// through, or toward either end of an axis, overwrites cells before it
// moves them (a move down the axis taken highest stripe first, or up it
// lowest first), moves the last part-stripe or another face's slab
// wrongly, or clears too few or too many of the cells that enter. The
// oracle is the world: in a row of 40 cells along each axis in turn, each
// face of each cell written with light 1 + its world index along the row
// and an eighth more for each face before it, scrolled 17 cells up the
// axis, then 9 down it (keeping 31, more than a stripe), then 45 up it,
// each face of each cell reads the light written at its world position
// where that cell has stayed in the volume throughout, and the fallback
// where it entered.
#[test]
fn a_scroll_across_several_stripes_keeps_every_cell_that_stays() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (mut renderer, mut scene) = fixture((&device, &queue));
    for axis in 0..3 {
        let along = Vec3::AXES[axis];
        let mut cells = [1; 3];
        cells[axis] = 40;
        let row = IrradianceVolume {
            origin: along * -20.,
            cell_size: Vec3::ONE,
            cells,
        };
        scene.set_irradiance_volume(&device, &queue, None).unwrap();
        scene
            .set_irradiance_volume(&device, &queue, Some(row))
            .unwrap();
        fill(&queue, &mut scene, row, |index| {
            faced(1. + index[axis] as f32)
        });
        // The world cells that hold what was written: those of the first
        // placement that stay in every later one.
        let mut held: Vec<i64> = (0..40).collect();
        let mut first = 0i64;
        for by in [17i64, -9, 45] {
            first += by;
            let placed = IrradianceVolume {
                origin: row.origin + along * first as f32,
                ..row
            };
            scene
                .set_irradiance_volume(&device, &queue, Some(placed))
                .unwrap();
            held.retain(|x| (first..first + 40).contains(x));
            let cells = every_cell(row.cells);
            let lights = faces(
                (&device, &queue),
                (&mut renderer, &mut scene),
                placed,
                &cells,
            );
            for (index, light) in cells.iter().zip(&lights) {
                let world = first + i64::from(index[axis]);
                let expected: [f32; 6] = if held.contains(&world) {
                    std::array::from_fn(|face| 1. + world as f32 + face as f32 / 8.)
                } else {
                    [0.; 6]
                };
                assert_eq!(*light, expected, "axis {axis}, after {by}: cell {index:?}");
            }
        }
    }
}

// Plausible defects: a move of the render origin that leaves the volume's
// origin where it was in the new frame, translates its cells, rounds its
// origin twice, or takes the placement it then holds, installed again, for
// another. The oracle is the world: after a move of (3.25, -1.5, 7.75),
// every cell reads its light at its old position less the move, and
// installing the placement the scene returns changes nothing.
#[test]
fn a_render_origin_move_keeps_the_field_where_it_was() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (mut renderer, mut scene) = fixture((&device, &queue));
    add_world((&device, &queue), &mut scene);
    let to = Vec3::new(3.25, -1.5, 7.75);
    scene.move_origin(&device, &queue, to).unwrap();
    let moved = scene.irradiance_volume().unwrap();
    assert!(
        (moved.origin - (WORLD.origin - to)).abs().max_element() < 1e-6,
        "{moved:?}"
    );
    scene
        .set_irradiance_volume(&device, &queue, Some(moved))
        .unwrap();
    let cells = every_cell(WORLD.cells);
    let queries: Vec<_> = cells
        .iter()
        .map(|&index| (floor_of(WORLD, index) - to, Vec3::Y, Vec3::Y))
        .collect();
    let mut input = input();
    input.camera.eye -= to;
    input.camera.view = Mat4::from_translation(to);
    let answers = field(
        (&device, &queue),
        (&mut renderer, &mut scene),
        &input,
        &queries,
    );
    for (index, answer) in cells.iter().zip(&answers) {
        // The +Y face, the third (`faced`).
        let expected = [world_light(index.map(i64::from)) + 2. / 8., 0.5, 1.];
        assert_eq!(*answer, expected, "cell {index:?}");
    }
}

// Plausible defects: no fade (a seam where the volume ends), a fade inside
// the extent, or over another width. The oracle is the architecture's: the
// share is whole within the extent (0 to 8 metres on each axis, cells of 2)
// and falls linearly to 0 over the one cell past each face, multiplied
// across the faces a point is past; beyond it the volume adds no light and
// takes nothing of the ambient.
#[test]
fn the_share_fades_over_the_cell_past_each_face() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (mut renderer, mut scene) = fixture((&device, &queue));
    let volume = IrradianceVolume {
        origin: Vec3::ZERO,
        cell_size: Vec3::splat(2.),
        cells: [4, 4, 4],
    };
    scene
        .set_irradiance_volume(&device, &queue, Some(volume))
        .unwrap();
    fill(&queue, &mut scene, volume, |_| cell(2., 0.5));
    // Each geometry normal lies across the axes the point is past.
    let points = [
        (Vec3::new(7.9, 4., 4.), Vec3::Y, 1.),
        (Vec3::new(9., 4., 4.), Vec3::Y, 0.5),
        (Vec3::new(10., 4., 4.), Vec3::Y, 0.),
        (Vec3::new(4., -1., 4.), Vec3::X, 0.5),
        (Vec3::new(9., -1., 4.), Vec3::Z, 0.25),
        (Vec3::new(4., 4., 11.), Vec3::Y, 0.),
    ];
    let queries: Vec<_> = points.iter().map(|&(p, g, _)| (p, g, Vec3::Y)).collect();
    let answers = field(
        (&device, &queue),
        (&mut renderer, &mut scene),
        &input(),
        &queries,
    );
    for ((point, _, share), answer) in points.iter().zip(&answers) {
        assert!(close(answer[2], *share, 1e-5), "{point}: {answer:?}");
    }
    assert_eq!(answers[2], [0., 1., 0.], "beyond the fade");
}

// Plausible defects: the volume lights charted receivers, sits below the
// dynamic GI volume or an ambient cube, leaves them their share where it
// covers them, hands over at its border with a gap or an overlap, or stays
// on with baked lighting off. The oracle is the architecture's
// determination: lightmap, atlas chart, irradiance volume, dynamic GI
// volume, ambient cube, frame's ambient. A dynamic GI volume over -3..3
// (an open one, which holds the uniform environment's radiance 0.5 from its
// first frame) and an irradiance volume over -3..0 on x (light 3,
// visibility 0.5): within both, a moving or uncharted static receiver
// takes the irradiance volume whole; a lightmapped one keeps its chart;
// past the irradiance volume, the dynamic GI volume takes over; half a
// cell past it, each takes half; beyond both, the moving receiver's cube;
// with baked lighting off, the irradiance volume and the cube give way to
// the dynamic GI volume. A floor below the receivers gives the dynamic GI
// probes about them a surface, as a static receiver's own surface gives its
// probes, so the static receivers take that volume too.
#[test]
fn the_volume_lights_receivers_below_charts_and_above_dynamic_gi_and_ambient_cubes() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let settings = Settings {
        dynamic_gi: DynamicGiQuality::High,
        ..settings()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let mut scene = Scene::new(&device, &queue);
    let environment = uniform_environment(gpu, &mut scene, 0.5);
    // Its top at y = -0.5, below the receivers and 1.5 m below the probes
    // at y = 1.
    let mut floor = test_support::cube();
    floor.materials[0].base = [0.5, 0.5, 0.5, 1.];
    floor.materials[0].metallic = 0.;
    let floor = scene.add_asset(&device, &queue, floor).unwrap().model;
    scene
        .add_instance(
            &device,
            &queue,
            InstanceState {
                model: floor,
                pose: Mat4::from_translation(Vec3::new(0., -1., 0.))
                    * Mat4::from_scale(Vec3::new(20., 1., 20.)),
                visible: true,
                capture_visible: true,
            },
            Mobility::Static,
        )
        .unwrap();
    scene
        .set_dynamic_gi_volume(
            &device,
            Some(DynamicGiVolume {
                origin: Vec3::splat(-3.),
                spacing: Vec3::splat(2.),
                probes: [4, 4, 4],
            }),
        )
        .unwrap();
    let volume = IrradianceVolume {
        origin: Vec3::splat(-3.),
        cell_size: Vec3::ONE,
        cells: [3, 6, 6],
    };
    scene
        .set_irradiance_volume(&device, &queue, Some(volume))
        .unwrap();
    fill(&queue, &mut scene, volume, |_| cell(3., 0.5));
    // (position, moving, lightmapped)
    let surfaces = [
        (Vec3::new(-1.5, 0., 0.), true, false),
        (Vec3::new(-1.5, 0., 0.), false, false),
        (Vec3::new(-1.5, 0., 0.), false, true),
        (Vec3::new(2., 0., 0.), true, false),
        (Vec3::new(0.5, 0., 0.), true, false),
        (Vec3::new(8., 0., 0.), true, false),
    ];
    let count = surfaces.len();
    let statements = format!(
        r#" let positions=array<vec3<f32>,{count}>({});
 let moving=array<bool,{count}>({});
 let lightmapped=array<bool,{count}>({});
 for (var i=0u;i<{count}u;i++) {{
  var s=fixture_surface(positions[i],vec3(0.,1.,0.),false,1.);
  s.moving=moving[i];
  s.baked=lightmapped[i];
  let indirect=surface_indirect_diffuse(s,s.normal,false);
  output[2u*i]=vec4(indirect.baked.r,indirect.field.r,indirect.dynamic_gi.r,indirect.dynamic_gi.a);
  output[2u*i+1u]=vec4(indirect.ambient,indirect.sky_visibility,0.,0.);
 }}"#,
        vec3s(surfaces.iter().map(|s| s.0)),
        surfaces.map(|s| s.1.to_string()).join(","),
        surfaces.map(|s| s.2.to_string()).join(","),
    );
    for baked_lighting in [true, false] {
        let mut input = input();
        input.environment = Some(environment);
        input.baked_lighting = baked_lighting;
        let output = crate::view::targets::target(&device, "frame", SIZE, gbuffer::COLOR);
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &input,
            &settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
        let answers = observe(
            gpu,
            (&mut renderer, &mut scene),
            &input,
            &settings,
            &statements,
            2 * count,
        );
        // (baked, field, dynamic GI's share, ambient, sky visibility); the
        // dynamic GI volume holds 0.5 wherever it has a share.
        let expected: [[f32; 5]; 6] = if baked_lighting {
            [
                [0., 3., 0., 0.5, 0.5],
                [0., 3., 0., 0.5, 0.5],
                [0., 0., 0., 1., 1.],
                [0., 0., 1., 0., 1.],
                [0., 1.5, 0.5, 0.25, 0.75],
                [0.25, 0., 0., 1., 1.],
            ]
        } else {
            [
                [0., 0., 1., 0., 1.],
                [0., 0., 1., 0., 1.],
                [0., 0., 1., 0., 1.],
                [0., 0., 1., 0., 1.],
                [0., 0., 1., 0., 1.],
                [0., 0., 0., 1., 1.],
            ]
        };
        for (i, expected) in expected.iter().enumerate() {
            let [baked, field, dynamic_gi, share] = answers[2 * i];
            let [ambient, sky_visibility, _, _] = answers[2 * i + 1];
            let actual = [baked, field, share, ambient, sky_visibility];
            assert!(
                actual
                    .iter()
                    .zip(expected)
                    .all(|(a, e)| close(*a, *e, 1e-4)),
                "baked lighting {baked_lighting}, {:?}: {actual:?}, expected {expected:?}",
                surfaces[i]
            );
            if share > 0. {
                assert!(
                    close(dynamic_gi, 0.5, 0.005),
                    "{:?}: {dynamic_gi}",
                    surfaces[i]
                );
            }
        }
    }
}

/// Floors facing +Y at the bottom of cells 0, 1 and 2 of a row of three
/// cells of a metre from the origin: light 0 and visibility 0.25, light 0.5
/// and visibility 0, and one never written; and one beyond the volume.
const ROW: IrradianceVolume = IrradianceVolume {
    origin: Vec3::ZERO,
    cell_size: Vec3::ONE,
    cells: [3, 1, 1],
};

fn add_row(gpu: Gpu, scene: &mut Scene) -> [Vec3; 4] {
    let (device, queue) = gpu;
    scene
        .set_irradiance_volume(device, queue, Some(ROW))
        .unwrap();
    let region =
        PreparedIrradianceRegion::new(ROW.origin, [2, 1, 1], &[cell(0., 0.25), cell(0.5, 0.)])
            .unwrap();
    scene.write_irradiance_cells(queue, &region).unwrap();
    [
        floor_of(ROW, [0, 0, 0]),
        floor_of(ROW, [1, 0, 0]),
        floor_of(ROW, [2, 0, 0]),
        Vec3::new(-5., 0., 0.5),
    ]
}

// Plausible defects: the sky visibility scales one ambient term and not the
// other, is applied as occlusion, or the own light takes the surface's
// environment scale, is dropped, or is damped at a dynamic GI probe's hit
// as the dynamic GI volume's own light is. The oracle is that a uniform
// environment's diffuse light is its radiance: for a grey receiver in a
// uniform environment of radiance 0.5, the ambient diffuse A beyond the
// volume becomes 0.25 A at visibility 0.25, A again under an own light of
// 0.5 with visibility 0, whatever the environment scale, which halves A
// beyond the volume, and A in a cell never written; a hemisphere fill
// scales by the visibility alike; and a probe ray's hit reflects an own
// light of 0.5 as it reflects the environment of 0.5.
#[test]
fn a_receiver_takes_the_volumes_light_and_the_ambient_its_sky_visibility_lets_through() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let (mut renderer, mut scene) = fixture(gpu);
    let environment = uniform_environment(gpu, &mut scene, 0.5);
    let [dim, lit, unwritten, beyond] = add_row(gpu, &mut scene);
    let cases = [
        (beyond, 1.),
        (dim, 1.),
        (lit, 1.),
        (lit, 0.5),
        (beyond, 0.5),
        (unwritten, 1.),
    ];
    let count = cases.len();
    let statements = format!(
        r#" let positions=array<vec3<f32>,{count}>({});
 let scales=array<f32,{count}>({});
 for (var i=0u;i<{count}u;i++) {{
  let s=fixture_surface(positions[i],vec3(0.,1.,0.),false,scales[i]);
  let camera=fixture_shaded(s,SHADOW_RECEIVER_CAPTURE,false);
  let hit=fixture_shaded(s,SHADOW_RECEIVER_PROBE_HIT,false);
  output[i]=vec4(camera.ambient.r,hit.color.r,camera.sky_visibility,0.);
 }}"#,
        vec3s(cases.iter().map(|c| c.0)),
        cases.map(|c| format!("{:?}", c.1)).join(","),
    );
    let mut input = input();
    input.environment = Some(environment);
    let answers = observe(
        gpu,
        (&mut renderer, &mut scene),
        &input,
        &settings(),
        &statements,
        count,
    );
    let ambient = answers[0][0];
    assert!(ambient > 0.05, "{answers:?}");
    let expected = [1., 0.25, 1., 1., 0.5, 1.];
    let visibility = [1., 0.25, 0., 0., 1., 1.];
    for (i, answer) in answers.iter().enumerate() {
        assert!(
            close(answer[0], ambient * expected[i], ambient * 0.01) && answer[2] == visibility[i],
            "case {:?}: {answer:?}, A {ambient}",
            cases[i]
        );
    }
    let hit = answers[0][1];
    assert!(
        close(answers[2][1], hit, hit * 0.01),
        "a probe hit damped the volume's own light: {} against {hit}",
        answers[2][1]
    );
    // The hemisphere fill alone.
    input.environment = None;
    input.hemisphere_light = HemisphereLight {
        sky_color: [1.; 3],
        ground_color: [1.; 3],
        intensity: 1.,
    };
    let answers = observe(
        gpu,
        (&mut renderer, &mut scene),
        &input,
        &settings(),
        &statements,
        count,
    );
    let fill = answers[0][0];
    assert!(fill > 0.05, "{answers:?}");
    assert!(
        close(answers[1][0], fill * 0.25, fill * 1e-4),
        "{answers:?}"
    );
    assert_eq!(answers[5][0], fill, "{answers:?}");
}

/// A probe of uniform radiance `radiance` whose influence spans 40 metres
/// about the origin, reflecting at infinity.
fn probe(radiance: f32) -> crate::BakedSpecularProbe {
    let face_size = 64;
    let texels: usize = (0..7)
        .map(|level| ((face_size >> level) as usize).pow(2) * 6)
        .sum();
    let half = test_support::to_half(radiance);
    crate::BakedSpecularProbe {
        center: Vec3::ZERO,
        world_to_local: Mat4::IDENTITY,
        influence: crate::SpecularProbeBox {
            min: Vec3::splat(-20.),
            max: Vec3::splat(20.),
        },
        blend: Vec3::ZERO,
        proxy: None,
        radiance: crate::SpecularProbeRadiance {
            face_size,
            texels: crate::SpecularProbeTexels::Rgba16Float(
                [half, half, half, test_support::to_half(1.)].repeat(texels),
            ),
        },
    }
}

// Plausible defects: the sky visibility leaves the sky's specular whole,
// occludes a probe's specular too, or changes shading where the volume
// lets the whole sky through. The oracle is Lagarde's occlusion at
// visibility 0, which is 0 for any lobe, and at 1, which applies none: a
// smooth white metal in a uniform environment reflects it beyond the
// volume, exactly as much in a cell never written, and nothing in a cell
// whose sky visibility is 0; with a specular probe about it, it reflects
// the probe there exactly as beyond the volume.
#[test]
fn the_sky_visibility_occludes_the_skys_specular_and_not_a_probes() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let (mut renderer, mut scene) = fixture(gpu);
    let environment = uniform_environment(gpu, &mut scene, 0.5);
    let [_, dark, unwritten, beyond] = add_row(gpu, &mut scene);
    // No diffuse environment, so the metal's multiscattering, which the
    // ambient's share scales, adds nothing: its light is its specular.
    let statements = format!(
        r#" let positions=array<vec3<f32>,3>({});
 for (var i=0u;i<3u;i++) {{
  let shaded=fixture_shaded(fixture_surface(positions[i],vec3(0.,1.,0.),true,1.),SHADOW_RECEIVER_CAPTURE,true);
  output[i]=vec4(shaded.color,shaded.sky_visibility);
 }}"#,
        vec3s([beyond, unwritten, dark]),
    );
    let mut input = input();
    input.environment = Some(environment);
    input.diffuse_environment.intensity = 0.;
    let mut observe = |scene: &mut Scene| {
        observe(
            gpu,
            (&mut renderer, scene),
            &input,
            &settings(),
            &statements,
            3,
        )
    };
    let [open, unwritten, dark]: [[f32; 4]; 3] = observe(&mut scene).try_into().unwrap();
    assert!(open[0] > 0.1, "{open:?}");
    assert_eq!(unwritten, open, "a cell never written");
    assert!(dark[0..3].iter().all(|&c| c.abs() < 1e-6), "{dark:?}");
    assert_eq!(dark[3], 0.);
    scene
        .set_baked_specular_probes(&device, &queue, &[probe(2.)])
        .unwrap();
    let [open, _, dark]: [[f32; 4]; 3] = observe(&mut scene).try_into().unwrap();
    assert!(open[0] > 0.5, "{open:?}");
    assert_eq!(dark[0..3], open[0..3], "the probe's specular was occluded");
}

/// The centre texel of `renderer`'s last frame's composite colour (rgb)
/// and the alpha of its ambient target.
fn centre(gpu: Gpu, renderer: &Renderer) -> [f32; 4] {
    let (device, queue) = gpu;
    let targets = renderer.targets();
    let at = ((SIZE[1] / 2) * SIZE[0] + SIZE[0] / 2) as usize * 8;
    let composite = test_support::read(device, queue, targets.composite.texture(), 8);
    let ambient = test_support::read(device, queue, targets.ambient.texture(), 8);
    let rgb: [f32; 3] =
        std::array::from_fn(|channel| test_support::half(&composite[at + channel * 2..]));
    [
        rgb[0],
        rgb[1],
        rgb[2],
        test_support::half(&ambient[at + 6..]),
    ]
}

// Plausible defects: the opaque pass leaves the ambient target's alpha at
// 0, or writes it where no volume lights a surface, so completion occludes
// the sky's specular everywhere; completion ignores it, or occludes probe
// specular by it. The oracle, in real frames of a smooth white metal wall
// facing the camera in a uniform environment, with no light: the wall
// reflects the environment as before in a volume never written, its
// ambient alpha 1, and nothing where the volume's sky visibility is 0, its
// alpha 0; with a specular probe about it, the probe exactly as without the
// volume.
#[test]
fn completion_occludes_the_skys_specular_by_the_visibility_the_opaque_pass_records() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let settings = settings();
    let frame = |volume: Option<f32>, probes: bool| -> [f32; 4] {
        let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
        let mut scene = Scene::new(&device, &queue);
        let environment = uniform_environment(gpu, &mut scene, 0.5);
        let mut wall = test_support::cube();
        let material = &mut wall.materials[0];
        material.base = [1.; 4];
        material.metallic = 1.;
        material.roughness = 0.1;
        let ids = scene.add_asset(&device, &queue, wall).unwrap();
        // The cube's face toward the camera lies at z = -6.
        scene
            .add_instance(
                &device,
                &queue,
                InstanceState {
                    model: ids.model,
                    pose: Mat4::from_translation(Vec3::new(0., 0., -16.))
                        * Mat4::from_scale(Vec3::splat(20.)),
                    visible: true,
                    capture_visible: true,
                },
                Mobility::Static,
            )
            .unwrap();
        if probes {
            scene
                .set_baked_specular_probes(&device, &queue, &[probe(2.)])
                .unwrap();
        }
        if let Some(visible) = volume {
            let placement = IrradianceVolume {
                origin: Vec3::splat(-8.),
                cell_size: Vec3::ONE,
                cells: [16, 16, 4],
            };
            scene
                .set_irradiance_volume(&device, &queue, Some(placement))
                .unwrap();
            if visible < 1. {
                fill(&queue, &mut scene, placement, |_| cell(0., visible));
            }
        }
        // No diffuse environment, so the opaque pass adds nothing: the
        // wall's light is what completion adds.
        let mut input = input();
        input.environment = Some(environment);
        input.diffuse_environment.intensity = 0.;
        let output = crate::view::targets::target(&device, "frame", SIZE, gbuffer::COLOR);
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &input,
            &settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
        centre(gpu, &renderer)
    };
    let open = frame(None, false);
    assert!(open[0] > 0.1 && open[3] == 1., "{open:?}");
    assert_eq!(frame(Some(1.), false), open, "a volume never written");
    let dark = frame(Some(0.), false);
    assert!(
        dark[0..3].iter().all(|&c| c.abs() < 1e-3) && dark[3] == 0.,
        "{dark:?}"
    );
    let probe = frame(None, true);
    assert!(probe[0] > open[0], "{probe:?}");
    let probe_dark = frame(Some(0.), true);
    assert_eq!(
        probe_dark[0..3],
        probe[0..3],
        "the probe's specular was occluded"
    );
}

// Plausible defects: a placement or region the scene cannot hold is taken,
// clipped, or half applied; the texture's limit is checked against the
// cells rather than Bevy's layout of them. The oracle is the API's: a
// placement that is not a lattice, or whose texture (twice the cells on y,
// three times on z) exceeds the device's 3D limit, is refused and keeps the
// installed one, while one at the limit is taken; a region whose corner,
// counts or values are invalid cannot be prepared; a write with no volume,
// off its lattice or partly outside it is refused and changes no cell.
#[test]
fn the_scene_refuses_what_it_cannot_hold_and_changes_nothing() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (mut renderer, mut scene) = fixture((&device, &queue));
    let region = PreparedIrradianceRegion::new(Vec3::ZERO, [1; 3], &[cell(5., 0.5)]).unwrap();
    assert!(matches!(
        scene.write_irradiance_cells(&queue, &region),
        Err(SceneError::IrradianceRegionOutside)
    ));
    let volume = IrradianceVolume {
        origin: Vec3::ZERO,
        cell_size: Vec3::ONE,
        cells: [2, 2, 2],
    };
    let limit = device.limits().max_texture_dimension_3d;
    for cells in [[limit, 1, 1], [1, limit / 2, 1], [1, 1, limit / 3]] {
        scene
            .set_irradiance_volume(&device, &queue, Some(IrradianceVolume { cells, ..volume }))
            .unwrap();
    }
    scene
        .set_irradiance_volume(&device, &queue, Some(volume))
        .unwrap();
    fill(&queue, &mut scene, volume, |_| cell(1., 1.));
    for invalid in [
        IrradianceVolume {
            origin: Vec3::new(f32::NAN, 0., 0.),
            ..volume
        },
        IrradianceVolume {
            cell_size: Vec3::new(1., 0., 1.),
            ..volume
        },
        IrradianceVolume {
            cell_size: Vec3::new(1., 1., -1.),
            ..volume
        },
        IrradianceVolume {
            cells: [2, 0, 2],
            ..volume
        },
    ] {
        assert!(
            matches!(
                scene.set_irradiance_volume(&device, &queue, Some(invalid)),
                Err(SceneError::InvalidIrradianceVolume)
            ),
            "{invalid:?}"
        );
    }
    for cells in [
        [limit + 1, 1, 1],
        [1, limit / 2 + 1, 1],
        [1, 1, limit / 3 + 1],
    ] {
        assert!(
            matches!(
                scene.set_irradiance_volume(
                    &device,
                    &queue,
                    Some(IrradianceVolume { cells, ..volume })
                ),
                Err(SceneError::DeviceLimit)
            ),
            "{cells:?}"
        );
    }
    assert_eq!(scene.irradiance_volume(), Some(volume));
    let cell_values = |light: f32, visible: f32| IrradianceCell {
        sky_visibility: [visible; 6],
        ..cell(light, 1.)
    };
    for (corner, cells, values) in [
        (Vec3::new(f32::INFINITY, 0., 0.), [1; 3], vec![cell(1., 1.)]),
        (Vec3::ZERO, [1, 0, 1], vec![]),
        (Vec3::ZERO, [2, 1, 1], vec![cell(1., 1.)]),
        (Vec3::ZERO, [1; 3], vec![cell(-1., 1.)]),
        (Vec3::ZERO, [1; 3], vec![cell(f32::NAN, 1.)]),
        (Vec3::ZERO, [1; 3], vec![cell(70000., 1.)]),
        (Vec3::ZERO, [1; 3], vec![cell_values(1., 1.5)]),
        (Vec3::ZERO, [1; 3], vec![cell_values(1., -0.1)]),
        (Vec3::ZERO, [1; 3], vec![cell_values(1., f32::NAN)]),
    ] {
        assert!(
            matches!(
                PreparedIrradianceRegion::new(corner, cells, &values),
                Err(SceneError::InvalidIrradianceRegion)
            ),
            "{corner}, {cells:?}"
        );
    }
    let five = vec![cell(5., 0.5); 2];
    for (corner, cells) in [
        (Vec3::new(0.3, 0., 0.), [1, 1, 2]),
        (Vec3::new(1., 0., 0.), [2, 1, 1]),
        (Vec3::new(0., -1., 0.), [1, 2, 1]),
        (Vec3::new(0., 0., 5.), [1, 1, 2]),
    ] {
        let region = PreparedIrradianceRegion::new(corner, cells, &five).unwrap();
        assert!(
            matches!(
                scene.write_irradiance_cells(&queue, &region),
                Err(SceneError::IrradianceRegionOutside)
            ),
            "{corner}, {cells:?}"
        );
    }
    let answers = floors(
        (&device, &queue),
        (&mut renderer, &mut scene),
        volume,
        &every_cell(volume.cells),
    );
    assert!(
        answers.iter().all(|answer| *answer == [1., 1., 1.]),
        "a refused write changed a cell: {answers:?}"
    );
}
