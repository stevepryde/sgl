//! The analytic side of a scenario: poses as exact functions of the frame
//! index, the camera a view gives them, and what the frame must hold where
//! a fixture's geometry projects. Pure CPU maths in SGL3D's conventions
//! (S3D-3: metres, +Y up, right-handed, camera forward -Z, `perspective`'s
//! reversed-Z infinite projection, column-major).
//!
//! Pixel positions are in pixels from the top-left corner, +y down: pixel
//! `[x, y]` spans `x..x + 1` by `y..y + 1` and its centre is at
//! `x + 0.5, y + 0.5`. UV is the position over the size.
use glam::{Mat4, Vec2, Vec3, Vec4};
use sgl_3d::{Camera, perspective};

/// The script's clock: frame `n` is `n / FRAME_RATE` seconds in.
pub const FRAME_RATE: f64 = 60.;

/// A rigid placement: a position and a turn about +Y.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub position: Vec3,
    pub yaw: f32,
}

impl Pose {
    pub fn at(position: Vec3) -> Self {
        Self { position, yaw: 0. }
    }

    pub fn matrix(self) -> Mat4 {
        Mat4::from_translation(self.position) * Mat4::from_rotation_y(self.yaw)
    }

    /// `local` carried by this pose: what is rigidly attached to it.
    pub fn then(self, local: Pose) -> Pose {
        Pose {
            position: self.position
                + Mat4::from_rotation_y(self.yaw).transform_vector3(local.position),
            yaw: self.yaw + local.yaw,
        }
    }
}

/// How a pose changes with the frame index, exactly: translating by
/// `velocity` metres and turning `yaw_rate` radians about +Y each frame,
/// and displaced at once by `jump`'s offset from its frame on (a camera
/// cut, an object's teleport).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Motion {
    pub velocity: Vec3,
    pub yaw_rate: f32,
    pub jump: Option<(u32, Vec3)>,
}

impl Motion {
    pub const STILL: Motion = Motion {
        velocity: Vec3::ZERO,
        yaw_rate: 0.,
        jump: None,
    };

    /// Translation by `velocity` metres a frame: a pan across the view, a
    /// dolly or an approach along it.
    pub fn translate(velocity: Vec3) -> Self {
        Self {
            velocity,
            ..Self::STILL
        }
    }

    /// A turn of `radians` a frame about +Y.
    pub fn yaw(radians: f32) -> Self {
        Self {
            yaw_rate: radians,
            ..Self::STILL
        }
    }

    /// Displaced by `by` from frame `frame` on.
    pub fn jumping_at(self, frame: u32, by: Vec3) -> Self {
        Self {
            jump: Some((frame, by)),
            ..self
        }
    }

    /// The pose at `frame` of what started at `start`.
    pub fn pose(&self, start: Pose, frame: u32) -> Pose {
        let steps = frame as f32;
        let jump = self
            .jump
            .filter(|&(at, _)| frame >= at)
            .map_or(Vec3::ZERO, |(_, by)| by);
        Pose {
            position: start.position + self.velocity * steps + jump,
            yaw: start.yaw + self.yaw_rate * steps,
        }
    }
}

/// A camera's view: its target size, vertical field of view and near plane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub size: [u32; 2],
    pub fov_y: f32,
    pub near: f32,
}

impl View {
    pub fn aspect(&self) -> f32 {
        self.size[0] as f32 / self.size[1] as f32
    }

    /// The same view at `factor` times the size.
    pub fn scaled(&self, factor: u32) -> View {
        View {
            size: self.size.map(|s| s * factor),
            ..*self
        }
    }

    /// The camera at `pose`, unjittered.
    pub fn camera(&self, pose: Pose) -> Camera {
        Camera {
            eye: pose.position,
            view: pose.matrix().inverse(),
            projection: perspective(self.fov_y, self.aspect(), self.near),
        }
    }

    /// The metres one pixel spans across the view `depth` metres ahead.
    pub fn pixel_metres(&self, depth: f32) -> f32 {
        2. * depth * (self.fov_y * 0.5).tan() / self.size[1] as f32
    }

    /// A camera pan: translation along the camera's starting right axis
    /// (+X) by `pixels` a frame at `depth` metres ahead.
    pub fn pan(&self, pixels: f32, depth: f32) -> Motion {
        Motion::translate(Vec3::X * pixels * self.pixel_metres(depth))
    }

    /// Where `point` projects through `camera`, in pixels, and its view
    /// depth in metres; `None` behind the camera.
    pub fn project(&self, camera: &Camera, point: Vec3) -> Option<(Vec2, f32)> {
        let view = camera.view.transform_point3(point);
        if view.z >= 0. {
            return None;
        }
        let clip = camera.projection * view.extend(1.);
        Some((
            self.ndc_to_pixel(Vec2::new(clip.x, clip.y) / clip.w),
            -view.z,
        ))
    }

    /// Where the world polygon `corners` projects through `camera`, in
    /// pixels; `None` if a corner lies behind the camera.
    pub fn project_polygon(&self, camera: &Camera, corners: &[Vec3]) -> Option<Vec<Vec2>> {
        corners
            .iter()
            .map(|&corner| self.project(camera, corner).map(|(pixel, _)| pixel))
            .collect()
    }

    /// The world direction from `camera` through the pixel position `pixel`.
    pub fn ray(&self, camera: &Camera, pixel: Vec2) -> Vec3 {
        let ndc = Vec2::new(
            pixel.x / self.size[0] as f32 * 2. - 1.,
            1. - pixel.y / self.size[1] as f32 * 2.,
        );
        let tan = (self.fov_y * 0.5).tan();
        let view = Vec3::new(ndc.x * tan * self.aspect(), ndc.y * tan, -1.);
        camera.view.inverse().transform_vector3(view).normalize()
    }

    fn ndc_to_pixel(&self, ndc: Vec2) -> Vec2 {
        Vec2::new(
            (ndc.x * 0.5 + 0.5) * self.size[0] as f32,
            (0.5 - ndc.y * 0.5) * self.size[1] as f32,
        )
    }

    fn uv(&self, pixel: Vec2) -> Vec2 {
        pixel / Vec2::new(self.size[0] as f32, self.size[1] as f32)
    }

    /// The `Motion` target's truth for the surface at the pixel position
    /// `pixel`, which lies on the plane of `corners` at `current` (model to
    /// world now) and was carried by `previous` (model to world in the last
    /// frame): current-minus-previous UV in xy and previous-minus-current
    /// view depth in z (G-buffer contract, `specs/sgl3d-architecture.md`).
    /// `None` where the pixel's ray misses the plane in front of the camera.
    #[allow(clippy::too_many_arguments)]
    pub fn surface_motion(
        &self,
        now: &Camera,
        before: &Camera,
        corners: [Vec3; 4],
        current: Mat4,
        previous: Mat4,
        pixel: Vec2,
    ) -> Option<Vec3> {
        let world = corners.map(|corner| current.transform_point3(corner));
        let normal = (world[1] - world[0]).cross(world[2] - world[0]);
        let direction = self.ray(now, pixel);
        let along = normal.dot(direction);
        if along == 0. {
            return None;
        }
        let distance = normal.dot(world[0] - now.eye) / along;
        if distance <= 0. {
            return None;
        }
        let point = now.eye + direction * distance;
        let was = previous.transform_point3(current.inverse().transform_point3(point));
        let (pixel_now, depth_now) = self.project(now, point)?;
        let (pixel_before, depth_before) = self.project(before, was)?;
        let uv = self.uv(pixel_now) - self.uv(pixel_before);
        Some(uv.extend(depth_before - depth_now))
    }

    /// The `Motion` target's RG truth for the sky at the pixel position
    /// `pixel`: the direction through it now, projected through the last
    /// frame's camera, whose translation does not move what is infinitely
    /// far.
    pub fn sky_motion(&self, now: &Camera, before: &Camera, pixel: Vec2) -> Vec2 {
        let direction = self.ray(now, pixel);
        let view = before.view.transform_vector3(direction);
        let clip: Vec4 = before.projection * view.extend(0.);
        let was = self.ndc_to_pixel(Vec2::new(clip.x, clip.y) / clip.w);
        self.uv(pixel) - self.uv(was)
    }
}

/// The share of the square about pixel `[x, y]`, grown by `margin` pixels on
/// every side, that `polygon` (pixel positions, convex, either winding)
/// covers: the pixel's box-filtered coverage at margin 0.
pub fn coverage(polygon: &[Vec2], pixel: [u32; 2], margin: f32) -> f32 {
    let low = Vec2::new(pixel[0] as f32, pixel[1] as f32) - margin;
    let high = low + 1. + 2. * margin;
    let (min, max) = polygon
        .iter()
        .fold((Vec2::INFINITY, Vec2::NEG_INFINITY), |(a, b), &p| {
            (a.min(p), b.max(p))
        });
    if max.x <= low.x || max.y <= low.y || min.x >= high.x || min.y >= high.y {
        return 0.;
    }
    // Sutherland–Hodgman against the square's four edges.
    let mut clipped = polygon.to_vec();
    let edges: [(Vec2, f32); 4] = [
        (Vec2::X, low.x),
        (-Vec2::X, -high.x),
        (Vec2::Y, low.y),
        (-Vec2::Y, -high.y),
    ];
    for (axis, offset) in edges {
        let inside = |p: Vec2| p.dot(axis) >= offset;
        let mut kept = Vec::with_capacity(clipped.len() + 1);
        for (i, &a) in clipped.iter().enumerate() {
            let b = clipped[(i + 1) % clipped.len()];
            if inside(a) {
                kept.push(a);
            }
            if inside(a) != inside(b) {
                let t = (offset - a.dot(axis)) / (b - a).dot(axis);
                kept.push(a + (b - a) * t);
            }
        }
        clipped = kept;
        if clipped.is_empty() {
            return 0.;
        }
    }
    let twice_area: f32 = (0..clipped.len())
        .map(|i| clipped[i].perp_dot(clipped[(i + 1) % clipped.len()]))
        .sum();
    (twice_area.abs() * 0.5 / (high - low).element_product()).min(1.)
}
