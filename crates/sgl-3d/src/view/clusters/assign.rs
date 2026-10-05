//! Light and decal assignment to a camera's clusters (`super::Clusters`).
//!
//! The grid and its assignment port Bevy 9d12036
//! `crates/bevy_light/src/cluster/mod.rs` (`ClusterConfig::FixedZ`,
//! `Clusters::update`) and `assign.rs` (`assign_objects_to_clusters`, with
//! Persson's iterative sphere refinement and Wronski's cone test for spot
//! lights, and its helpers), MIT OR Apache-2.0 (`src/LICENSE-bevy.txt`).
//! Changes: point, spot and rectangle lights share one list per cluster,
//! live lights first, then decals, each a sphere about its box, as Bevy
//! assigns clustered decals; a rectangle takes the spot test at a right angle,
//! which keeps the clusters in front of its face, where Bevy assigns it as a
//! point light; the last slice ends at this frame's farthest light rather than the
//! previous frame's, and at least at twice the first slice's depth (a metre
//! past it, orthographic), where Bevy divides by `ln(far / near) = 0` when
//! every light ends within the first slice; the frustum has no far plane, as
//! the camera's projection has none; a cluster's bounds for the cone test use
//! the same tiles as its planes and the shader's lookup (Bevy's round tiles
//! up); the view is rigid, so radii keep their length. And the refinement finds the
//! slice and row a light's center lies beyond by its signed distance to
//! their planes in view space. Bevy projects the center to find them, which
//! mirrors a center behind the camera and misses clusters its sphere
//! reaches in front, and it cuts a center between the camera and the near
//! plane at the camera's plane, which is not conservative.
use crate::content::decal::Decal;
use crate::content::light::{Light, LightShape};
use crate::shading::clusters::ClusterGrid;
use glam::{Mat4, UVec2, UVec3, Vec2, Vec3, Vec4, Vec4Swizzles};

/// Bevy's `ClusterConfig::FixedZ`: at most `total` clusters, `z_slices` of
/// them in depth and the rest square on the screen, the first slice
/// reaching `first_slice_depth` metres.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClusterConfig {
    pub total: u32,
    pub z_slices: u32,
    pub first_slice_depth: f32,
}

/// The camera's grid: Bevy's 4096 clusters and first slice, with twice its
/// 24 depth slices. Measured on Hyperdrive's tunnel route, the lights whose
/// range reaches a pixel set most of its cost; twice the slices took 2% off
/// opaque lighting there at no CPU cost, and four times the clusters 5% for
/// about a sixth more CPU time per frame.
pub(crate) const CAMERA_CLUSTERS: ClusterConfig = ClusterConfig {
    total: 4096,
    z_slices: 48,
    first_slice_depth: 5.,
};

impl ClusterConfig {
    /// `ClusterConfig::dimensions_for_screen_size`.
    fn dimensions_for_screen_size(&self, screen_size: UVec2) -> UVec3 {
        let aspect_ratio = screen_size.x as f32 / screen_size.y as f32;
        let z_slices = self.z_slices.min(self.total);
        let per_layer = self.total as f32 / z_slices as f32;
        let y = (per_layer / aspect_ratio).sqrt();
        let mut x = (y * aspect_ratio) as u32;
        let mut y = y as u32;
        if x == 0 {
            x = 1;
            y = per_layer as u32;
        }
        if y == 0 {
            x = per_layer as u32;
            y = 1;
        }
        UVec3::new(x, y, z_slices)
    }
}

/// A light or decal as the assignment sees it.
#[derive(Clone, Copy)]
pub(super) struct Clusterable {
    /// Its index in the scene's light or decal buffer, marked with its kind
    /// (`super::BAKED`, `super::DECAL`).
    pub index: u32,
    sphere: Sphere,
    /// A spot's direction (toward where it shines) and outer angle; a
    /// rectangle's normal and a right angle, the half-space in front of its
    /// face.
    spot: Option<(Vec3, f32)>,
}

impl Clusterable {
    /// `light`, its range a sphere about it.
    pub fn light(index: u32, light: &Light) -> Self {
        Self {
            index,
            sphere: Sphere {
                center: light.position,
                radius: light.range,
            },
            spot: match light.shape {
                LightShape::Point { .. } => None,
                LightShape::Spot {
                    direction,
                    outer_angle,
                    ..
                } => Some((direction.normalize(), outer_angle)),
                LightShape::Rect { direction, .. } => {
                    Some((direction.normalize(), std::f32::consts::FRAC_PI_2))
                }
            },
        }
    }

    /// `decal`, the sphere that bounds its box: Bevy's decal range, the
    /// length of its scale, is its box's whole diagonal, twice this.
    pub fn decal(index: u32, decal: &Decal) -> Self {
        Self {
            index,
            sphere: decal_sphere(decal),
            spot: None,
        }
    }
}

/// The sphere that bounds `decal`'s box.
fn decal_sphere(decal: &Decal) -> Sphere {
    Sphere {
        center: decal.position,
        radius: 0.5 * decal.size.length(),
    }
}

#[derive(Clone, Copy, Debug)]
struct Sphere {
    center: Vec3,
    radius: f32,
}

/// Bevy's `HalfSpace`: a plane through `normal_d`, normalised; a point `p`
/// is inside where `normal · p + d > 0`.
#[derive(Clone, Copy)]
struct HalfSpace(Vec4);

impl HalfSpace {
    fn new(normal_d: Vec4) -> Self {
        Self(normal_d * normal_d.xyz().length_recip())
    }
    fn normal(&self) -> Vec3 {
        self.0.xyz()
    }
    fn d(&self) -> f32 {
        self.0.w
    }
}

/// The view's clip volume without a far plane (Bevy's
/// `ViewFrustum::from_clip_from_world_no_far`): left, right, bottom, top and
/// the reversed-Z near plane.
fn frustum(clip_from_world: Mat4) -> [HalfSpace; 5] {
    let row = |i| clip_from_world.row(i);
    let row3 = row(3);
    [
        HalfSpace::new(row3 + row(0)),
        HalfSpace::new(row3 - row(0)),
        HalfSpace::new(row3 + row(1)),
        HalfSpace::new(row3 - row(1)),
        HalfSpace::new(row3 - row(2)),
    ]
}

/// `Frustum::intersects_sphere`.
fn intersects_sphere(frustum: &[HalfSpace; 5], sphere: &Sphere) -> bool {
    let center = sphere.center.extend(1.);
    frustum
        .iter()
        .all(|half_space| half_space.0.dot(center) + sphere.radius > 0.)
}

/// A view's clip volume without a far plane, for culling lights by their
/// range, as the camera's assignment culls them and Wicked Engine culls the
/// frame's light list.
pub(crate) struct ViewVolume([HalfSpace; 5]);

impl ViewVolume {
    pub fn new(clip_from_world: Mat4) -> Self {
        Self(frustum(clip_from_world))
    }

    /// Whether `light`'s range reaches into the volume.
    pub fn reaches(&self, light: &Light) -> bool {
        intersects_sphere(
            &self.0,
            &Sphere {
                center: light.position,
                radius: light.range,
            },
        )
    }

    /// Whether the sphere about `decal`'s box reaches into the volume.
    pub fn reaches_decal(&self, decal: &Decal) -> bool {
        intersects_sphere(&self.0, &decal_sphere(decal))
    }
}

/// A box, for culling lights by their range as the dynamic GI volume's list
/// culls them against its extent: a sphere reaches it where its centre lies
/// within its radius of the box's nearest point.
pub(crate) struct BoxVolume {
    pub min: Vec3,
    pub max: Vec3,
}

impl BoxVolume {
    fn reaches_sphere(&self, sphere: &Sphere) -> bool {
        let nearest = sphere.center.clamp(self.min, self.max);
        nearest.distance_squared(sphere.center) <= sphere.radius * sphere.radius
    }

    /// Whether `light`'s range reaches into the box.
    pub fn reaches(&self, light: &Light) -> bool {
        self.reaches_sphere(&Sphere {
            center: light.position,
            radius: light.range,
        })
    }

    /// Whether the sphere about `decal`'s box reaches into the box.
    pub fn reaches_decal(&self, decal: &Decal) -> bool {
        self.reaches_sphere(&decal_sphere(decal))
    }
}

/// Scratch kept between frames so assignment allocates nothing once warm.
#[derive(Default)]
pub(super) struct Scratch {
    /// The lights and decals to assign: live lights, baked lights, decals.
    pub objects: Vec<Clusterable>,
    visible: Vec<(Clusterable, Vec3)>,
    /// (cluster, item) pairs, in `objects`' order.
    pub pairs: Vec<(u32, u32)>,
    spheres: Vec<Option<Sphere>>,
    pub cursor: Vec<u32>,
    x_planes: Vec<HalfSpace>,
    y_planes: Vec<HalfSpace>,
    z_planes: Vec<HalfSpace>,
}

/// `assign_objects_to_clusters` for one view: `scratch.objects` into the
/// clusters of `view` and `projection` at `screen` pixels, as (cluster,
/// item) pairs in `scratch.pairs`, each item its object's marked index.
/// Returns the grid.
pub(super) fn assign(
    view_from_world: Mat4,
    clip_from_view: Mat4,
    screen_size: UVec2,
    config: ClusterConfig,
    scratch: &mut Scratch,
) -> ClusterGrid {
    let requested_cluster_dimensions = config.dimensions_for_screen_size(screen_size);
    // `Clusters::update`.
    let tile_size = (screen_size.as_vec2() / requested_cluster_dimensions.truncate().as_vec2())
        .ceil()
        .as_uvec2()
        .max(UVec2::ONE);
    let dimensions = (screen_size.as_vec2() / tile_size.as_vec2())
        .ceil()
        .as_uvec2()
        .extend(requested_cluster_dimensions.z)
        .max(UVec3::ONE);
    let is_orthographic = clip_from_view.w_axis.w == 1.0;
    let clip_from_world = clip_from_view * view_from_world;
    let frustum = frustum(clip_from_world);

    // The lights and decals in the view, and the farthest depth one reaches
    // (`ClusterFarZMode::MaxClusterableObjectRange`, from this frame).
    let view_from_world_row_2 = view_from_world.row(2);
    let mut farthest_z = 0.0f32;
    scratch.visible.clear();
    for object in &scratch.objects {
        if !intersects_sphere(&frustum, &object.sphere) {
            continue;
        }
        farthest_z = farthest_z.max(
            -view_from_world_row_2.dot(object.sphere.center.extend(1.)) + object.sphere.radius,
        );
        // The center's view position, kept beside the object rather than
        // recomputed.
        let center = view_from_world.transform_point3(object.sphere.center);
        scratch.visible.push((*object, center));
    }

    let first_slice_depth = match (is_orthographic, dimensions.z) {
        (true, _) => (clip_from_view.w_axis.z - 1.0) / clip_from_view.z_axis.z,
        (false, 1) => config.first_slice_depth.max(farthest_z),
        _ => config.first_slice_depth,
    };
    // The last slice ends past the first, so the slicing never divides by a
    // zero logarithm (or depth range).
    let far_z = farthest_z.max(if is_orthographic {
        first_slice_depth + 1.0
    } else {
        2.0 * first_slice_depth
    });
    let cluster_factors = calculate_cluster_factors(
        first_slice_depth,
        far_z,
        dimensions.z as f32,
        is_orthographic,
    );
    let grid = ClusterGrid {
        dimensions: dimensions.to_array(),
        orthographic: u32::from(is_orthographic),
        factors: [
            dimensions.x as f32 / screen_size.x as f32,
            dimensions.y as f32 / screen_size.y as f32,
            cluster_factors.x,
            cluster_factors.y,
        ],
    };
    let cluster_count = (dimensions.x * dimensions.y * dimensions.z) as usize;
    let view_from_clip = clip_from_view.inverse();
    let tile = screen_size.as_vec2() / dimensions.truncate().as_vec2();

    scratch.spheres.clear();
    scratch.spheres.resize(cluster_count, None);
    scratch.pairs.clear();
    let x_planes = &mut scratch.x_planes;
    let y_planes = &mut scratch.y_planes;
    let z_planes = &mut scratch.z_planes;
    x_planes.clear();
    y_planes.clear();
    z_planes.clear();

    // The x, y and z cluster planes in view space.
    if is_orthographic {
        for x in 0..=dimensions.x {
            let x_pos = x as f32 / dimensions.x as f32 * 2.0 - 1.0;
            let view_x = clip_to_view(view_from_clip, Vec4::new(x_pos, 0.0, 1.0, 1.0)).x;
            x_planes.push(HalfSpace::new(Vec3::X.extend(view_x)));
        }
        for y in 0..=dimensions.y {
            let y_pos = (1.0 - y as f32 / dimensions.y as f32) * 2.0 - 1.0;
            let view_y = clip_to_view(view_from_clip, Vec4::new(0.0, y_pos, 1.0, 1.0)).y;
            y_planes.push(HalfSpace::new(Vec3::Y.extend(view_y)));
        }
    } else {
        for x in 0..=dimensions.x {
            let x_pos = x as f32 / dimensions.x as f32 * 2.0 - 1.0;
            let nb = clip_to_view(view_from_clip, Vec4::new(x_pos, -1.0, 1.0, 1.0)).xyz();
            let nt = clip_to_view(view_from_clip, Vec4::new(x_pos, 1.0, 1.0, 1.0)).xyz();
            let normal = nb.cross(nt);
            x_planes.push(HalfSpace::new(normal.extend(nb.dot(normal))));
        }
        for y in 0..=dimensions.y {
            let y_pos = (1.0 - y as f32 / dimensions.y as f32) * 2.0 - 1.0;
            let nl = clip_to_view(view_from_clip, Vec4::new(-1.0, y_pos, 1.0, 1.0)).xyz();
            let nr = clip_to_view(view_from_clip, Vec4::new(1.0, y_pos, 1.0, 1.0)).xyz();
            let normal = nr.cross(nl);
            y_planes.push(HalfSpace::new(normal.extend(nr.dot(normal))));
        }
    }
    for z in 0..=dimensions.z {
        let view_z = z_slice_to_view_z(first_slice_depth, far_z, dimensions.z, z, is_orthographic);
        let normal = -Vec3::Z;
        z_planes.push(HalfSpace::new(normal.extend(view_z * normal.z)));
    }

    for &(object, view_center) in &scratch.visible {
        let (min_ndc, max_ndc) = cluster_space_clusterable_object_aabb(
            view_center,
            object.sphere.radius,
            clip_from_view,
        );
        let min_cluster = ndc_position_to_cluster(
            dimensions,
            cluster_factors,
            is_orthographic,
            min_ndc,
            min_ndc.z,
        );
        let max_cluster = ndc_position_to_cluster(
            dimensions,
            cluster_factors,
            is_orthographic,
            max_ndc,
            max_ndc.z,
        );
        let (min_cluster, max_cluster) =
            (min_cluster.min(max_cluster), max_cluster.max(min_cluster));

        // Persson et al., Practical Clustered Shading (the Iterative Sphere
        // Refinement of Just Cause 3): a sphere under perspective is no longer
        // a sphere, so its widest extent on an axis is not its center plus its
        // radius.
        let view_sphere = Sphere {
            center: view_center,
            radius: object.sphere.radius,
        };
        let spot = object.spot.map(|(direction, outer_angle)| {
            let (angle_sin, angle_cos) = outer_angle.sin_cos();
            // Bevy's spot test runs along the light's back, away from where
            // it shines.
            (
                view_from_world.transform_vector3(-direction).normalize(),
                angle_sin,
                angle_cos,
            )
        });
        for z in min_cluster.z..=max_cluster.z {
            // Outside its slice, the slice's plane nearer the light cuts the
            // larger circle from its sphere.
            let (near, far) = (z_planes[z as usize], z_planes[z as usize + 1]);
            let z_plane = if z_distance(near, view_center) > 0. {
                Some(near)
            } else if z_distance(far, view_center) < 0. {
                Some(far)
            } else {
                None
            };
            let z_object = match z_plane {
                None => view_sphere,
                Some(plane) => match project_to_plane_z(view_sphere, plane) {
                    Some(projected) => projected,
                    None => continue,
                },
            };
            for y in min_cluster.y..=max_cluster.y {
                let (top, bottom) = (y_planes[y as usize], y_planes[y as usize + 1]);
                let y_plane = if get_distance_y(top, z_object.center, is_orthographic) > 0. {
                    Some(top)
                } else if get_distance_y(bottom, z_object.center, is_orthographic) < 0. {
                    Some(bottom)
                } else {
                    None
                };
                let y_object = match y_plane {
                    None => z_object,
                    Some(plane) => match project_to_plane_y(z_object, plane, is_orthographic) {
                        Some(projected) => projected,
                        None => continue,
                    },
                };
                // The first and last clusters the refined sphere reaches.
                let mut min_x = min_cluster.x;
                while min_x < max_cluster.x
                    && -get_distance_x(
                        x_planes[(min_x + 1) as usize],
                        y_object.center,
                        is_orthographic,
                    ) + y_object.radius
                        <= 0.0
                {
                    min_x += 1;
                }
                let mut max_x = max_cluster.x;
                while max_x > min_x
                    && get_distance_x(x_planes[max_x as usize], y_object.center, is_orthographic)
                        + y_object.radius
                        <= 0.0
                {
                    max_x -= 1;
                }
                let mut cluster_index = (y * dimensions.x + min_x) * dimensions.z + z;
                for x in min_x..=max_x {
                    let reaches = match spot {
                        None => true,
                        Some((view_back, angle_sin, angle_cos)) => {
                            // Wronski, "Cull that cone", against the
                            // cluster's bounding sphere.
                            let cluster_sphere = *scratch.spheres[cluster_index as usize]
                                .get_or_insert_with(|| {
                                    let (min, max) = compute_aabb_for_cluster(
                                        first_slice_depth,
                                        far_z,
                                        tile,
                                        screen_size.as_vec2(),
                                        view_from_clip,
                                        is_orthographic,
                                        dimensions,
                                        UVec3::new(x, y, z),
                                    );
                                    Sphere {
                                        center: (min + max) * 0.5,
                                        radius: ((max - min) * 0.5).length(),
                                    }
                                });
                            let offset = view_sphere.center - cluster_sphere.center;
                            let distance_squared = offset.length_squared();
                            let v1_len = offset.dot(view_back);
                            let distance_closest_point = angle_cos
                                * (distance_squared - v1_len * v1_len).max(0.).sqrt()
                                - v1_len * angle_sin;
                            let angle_cull = distance_closest_point > cluster_sphere.radius;
                            let front_cull = v1_len > cluster_sphere.radius + view_sphere.radius;
                            let back_cull = v1_len < -cluster_sphere.radius;
                            !angle_cull && !front_cull && !back_cull
                        }
                    };
                    if reaches {
                        scratch.pairs.push((cluster_index, object.index));
                    }
                    cluster_index += dimensions.z;
                }
            }
        }
    }

    grid
}

/// `calculate_cluster_factors`: the z slicing's scale and offset.
fn calculate_cluster_factors(near: f32, far: f32, z_slices: f32, is_orthographic: bool) -> Vec2 {
    if is_orthographic {
        Vec2::new(-near, z_slices / (-far - -near))
    } else {
        let z_slices_of_ln_zfar_over_znear = (z_slices - 1.0) / (far / near).ln();
        Vec2::new(
            z_slices_of_ln_zfar_over_znear,
            near.ln() * z_slices_of_ln_zfar_over_znear,
        )
    }
}

/// `compute_aabb_for_cluster`, with `tile` the screen divided by the grid.
#[allow(clippy::too_many_arguments)]
fn compute_aabb_for_cluster(
    z_near: f32,
    z_far: f32,
    tile: Vec2,
    screen_size: Vec2,
    view_from_clip: Mat4,
    is_orthographic: bool,
    cluster_dimensions: UVec3,
    ijk: UVec3,
) -> (Vec3, Vec3) {
    let ijk = ijk.as_vec3();
    let p_min = ijk.truncate() * tile;
    let p_max = p_min + tile;
    if is_orthographic {
        // Reversed Z: 1 is the near plane.
        let mut p_min = screen_to_view(screen_size, view_from_clip, p_min, 0.0).xyz();
        let mut p_max = screen_to_view(screen_size, view_from_clip, p_max, 0.0).xyz();
        p_min.z = -z_near + (z_near - z_far) * ijk.z / cluster_dimensions.z as f32;
        p_max.z = -z_near + (z_near - z_far) * (ijk.z + 1.0) / cluster_dimensions.z as f32;
        (p_min.min(p_max), p_min.max(p_max))
    } else {
        let p_min = screen_to_view(screen_size, view_from_clip, p_min, 1.0).xyz();
        let p_max = screen_to_view(screen_size, view_from_clip, p_max, 1.0).xyz();
        let z_far_over_z_near = -z_far / -z_near;
        let cluster_near = if ijk.z == 0.0 {
            0.0
        } else {
            -z_near * z_far_over_z_near.powf((ijk.z - 1.0) / (cluster_dimensions.z - 1) as f32)
        };
        let cluster_far = if cluster_dimensions.z == 1 {
            -z_far
        } else {
            -z_near * z_far_over_z_near.powf(ijk.z / (cluster_dimensions.z - 1) as f32)
        };
        let p_min_near = line_intersection_to_z_plane(p_min, cluster_near);
        let p_min_far = line_intersection_to_z_plane(p_min, cluster_far);
        let p_max_near = line_intersection_to_z_plane(p_max, cluster_near);
        let p_max_far = line_intersection_to_z_plane(p_max, cluster_far);
        (
            p_min_near.min(p_min_far).min(p_max_near.min(p_max_far)),
            p_min_near.max(p_min_far).max(p_max_near.max(p_max_far)),
        )
    }
}

/// `z_slice_to_view_z`, the inverse of the shader's z slicing.
fn z_slice_to_view_z(
    near: f32,
    far: f32,
    z_slices: u32,
    z_slice: u32,
    is_orthographic: bool,
) -> f32 {
    if is_orthographic {
        return -near - (far - near) * z_slice as f32 / z_slices as f32;
    }
    if z_slice == 0 {
        0.0
    } else {
        -near * (far / near).powf((z_slice - 1) as f32 / (z_slices - 1) as f32)
    }
}

fn ndc_position_to_cluster(
    cluster_dimensions: UVec3,
    cluster_factors: Vec2,
    is_orthographic: bool,
    ndc_p: Vec3,
    view_z: f32,
) -> UVec3 {
    let frag_coord =
        (ndc_p.truncate() * Vec2::new(0.5, -0.5) + Vec2::splat(0.5)).clamp(Vec2::ZERO, Vec2::ONE);
    let xy = (frag_coord * cluster_dimensions.truncate().as_vec2()).floor();
    let z_slice = view_z_to_z_slice(
        cluster_factors,
        cluster_dimensions.z,
        view_z,
        is_orthographic,
    );
    xy.as_uvec2()
        .extend(z_slice)
        .clamp(UVec3::ZERO, cluster_dimensions - UVec3::ONE)
}

/// `cluster_space_clusterable_object_aabb`: the light's bounds with x and y
/// in normalised device coordinates and z in view space.
fn cluster_space_clusterable_object_aabb(
    view_center: Vec3,
    radius: f32,
    clip_from_view: Mat4,
) -> (Vec3, Vec3) {
    let mut view_min = view_center - Vec3::splat(radius);
    let mut view_max = view_center + Vec3::splat(radius);
    // Keep view z in front of the camera, where perspective keeps the axes'
    // directions.
    view_min.z = view_min.z.min(-f32::MIN_POSITIVE);
    view_max.z = view_max.z.min(-f32::MIN_POSITIVE);
    // The nearer and farther z at the minimum and maximum x and y: under
    // perspective either may be the wider on screen.
    let corners = [
        view_min,
        view_min.truncate().extend(view_max.z),
        view_max.truncate().extend(view_min.z),
        view_max,
    ]
    .map(|corner| {
        let clip = clip_from_view * corner.extend(1.0);
        clip.xyz() / clip.w
    });
    let ndc_min = corners[0].min(corners[1]).min(corners[2]).min(corners[3]);
    let ndc_max = corners[0].max(corners[1]).max(corners[2]).max(corners[3]);
    (
        ndc_min
            .truncate()
            .clamp(-Vec2::ONE, Vec2::ONE)
            .extend(view_min.z),
        ndc_max
            .truncate()
            .clamp(-Vec2::ONE, Vec2::ONE)
            .extend(view_max.z),
    )
}

/// The ray from the eye through `p` at view depth `z`.
fn line_intersection_to_z_plane(p: Vec3, z: f32) -> Vec3 {
    p * (z / p.z)
}

/// `view_z_to_z_slice`, as the shader's lookup slices.
fn view_z_to_z_slice(
    cluster_factors: Vec2,
    z_slices: u32,
    view_z: f32,
    is_orthographic: bool,
) -> u32 {
    let z_slice = if is_orthographic {
        ((view_z - cluster_factors.x) * cluster_factors.y).floor() as u32
    } else {
        ((-view_z).ln() * cluster_factors.x - cluster_factors.y + 1.0) as u32
    };
    z_slice.min(z_slices - 1)
}

fn clip_to_view(view_from_clip: Mat4, clip: Vec4) -> Vec4 {
    let view = view_from_clip * clip;
    view / view.w
}

fn screen_to_view(screen_size: Vec2, view_from_clip: Mat4, screen: Vec2, ndc_z: f32) -> Vec4 {
    let tex_coord = screen / screen_size;
    let clip = Vec4::new(
        tex_coord.x * 2.0 - 1.0,
        (1.0 - tex_coord.y) * 2.0 - 1.0,
        ndc_z,
        1.0,
    );
    clip_to_view(view_from_clip, clip)
}

/// Signed distance to an x plane, whose normal has no y: positive to its
/// right.
fn get_distance_x(plane: HalfSpace, point: Vec3, is_orthographic: bool) -> f32 {
    if is_orthographic {
        point.x - plane.d()
    } else {
        plane.0.x * point.x + plane.0.z * point.z
    }
}

/// Signed distance to a y plane, whose normal has no x: positive above it,
/// toward the rows before it.
fn get_distance_y(plane: HalfSpace, point: Vec3, is_orthographic: bool) -> f32 {
    if is_orthographic {
        point.y - plane.d()
    } else {
        plane.0.y * point.y + plane.0.z * point.z
    }
}

/// Signed distance to a z plane: positive toward the camera, the slices
/// before it.
fn z_distance(plane: HalfSpace, point: Vec3) -> f32 {
    point.z - plane.d() / plane.normal().z
}

/// The circle a z plane cuts from a sphere, as a sphere.
fn project_to_plane_z(z_object: Sphere, z_plane: HalfSpace) -> Option<Sphere> {
    let z = z_plane.d() / z_plane.normal().z;
    let distance_to_plane = z - z_object.center.z;
    if distance_to_plane.abs() > z_object.radius {
        return None;
    }
    Some(Sphere {
        center: z_object.center.truncate().extend(z),
        radius: (z_object.radius * z_object.radius - distance_to_plane * distance_to_plane).sqrt(),
    })
}

/// The circle a y plane cuts from a sphere, as a sphere.
fn project_to_plane_y(
    y_object: Sphere,
    y_plane: HalfSpace,
    is_orthographic: bool,
) -> Option<Sphere> {
    let distance_to_plane = if is_orthographic {
        y_plane.d() - y_object.center.y
    } else {
        -(y_object.center.y * y_plane.0.y + y_object.center.z * y_plane.0.z)
    };
    if distance_to_plane.abs() > y_object.radius {
        return None;
    }
    Some(Sphere {
        center: y_object.center + distance_to_plane * y_plane.normal(),
        radius: (y_object.radius * y_object.radius - distance_to_plane * distance_to_plane).sqrt(),
    })
}
