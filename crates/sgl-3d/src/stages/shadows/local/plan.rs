//! What the local-light shadow atlas draws in a frame: which lights cast,
//! where their faces are, which faces changed, and each light's shadow
//! record.
//!
//! The lights that cast a shadow and reach the camera's view are ranked by
//! screen coverage and placed in that order (`atlas`): Godot places lights in
//! cull order, and ranking them gives the room to the lights that matter
//! most. A face is drawn only when what it shows changed (`cache`): its
//! static layer when the face is new to its slot, the visibility mask or a
//! material's caster values changed, or a static edit reached it; its moving
//! casters over a copy of that layer when they entered, left or moved. A
//! light that moved since the last finished frame has no reusable layer and
//! draws every caster at once.
use super::LocalShadowStats;
use super::atlas::{self, Atlas, Placement};
use super::cache::{Cache, FaceKey, MovingCaster, Slot};
use super::shape::{LightView, Shape};
use crate::content::identity::{Identity, LightId};
use crate::content::light::Light;
use crate::scene::static_edits::posed_bounds;
use crate::shading::lights::{LOCAL_SHADOW_CUBE, LOCAL_SHADOW_SPOT, LocalShadowRecord};
use crate::view::clusters::ViewVolume;
use crate::view::culling::clip_intersects;
use crate::view::draw_list::{DrawInstances, DrawList};
use crate::view::population::{Casters, LightReach, Population, moving_caster_reaches, within};
use crate::view::{LOCAL_SHADOW_NEAR, View};
use crate::{Mobility, Scene};
use glam::{Mat4, Vec3};
use std::collections::HashMap;
use std::ops::Range;

/// A moving caster with its bounds in its model's space and in the world.
type Moving = (MovingCaster, [Vec3; 2], [Vec3; 2]);

/// What a face draws this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Work {
    /// Every caster, into the frame atlas: its light moved.
    Full,
    /// The static layer, then a copy of it and the moving casters.
    Layer,
    /// A copy of its static layer and the moving casters.
    Moving,
}

/// One face drawn this frame.
pub(super) struct Face {
    pub work: Work,
    /// Its slot's top-left texel and size.
    pub origin: [u32; 2],
    pub size: u32,
    pub view: View,
    /// Its static casters, or every caster for `Work::Full`.
    pub casters: DrawList,
    /// Its moving casters.
    pub moving: DrawList,
}

pub(super) struct Plan {
    allocation: Atlas,
    cache: Cache,
    /// Frames prepared.
    frame: u64,
    faces: Vec<Face>,
    /// How many of `faces` this frame draws.
    drawn: usize,
    /// Each light's shadow record, at its index.
    records: Vec<LocalShadowRecord>,
    /// The static cluster groups the light being planned reaches.
    reach: LightReach,
    /// Marking static edits: each held light's position and range, and
    /// where in `near_edits` the indices of the edits within it are.
    edit_lights: HashMap<LightId, ((Vec3, f32), Range<usize>)>,
    near_edits: Vec<usize>,
    pub stats: LocalShadowStats,
}

impl Plan {
    /// Nothing placed or drawn yet in an atlas of `size` texels a side.
    pub fn new(size: u32) -> Self {
        Self {
            allocation: Atlas::new(size),
            cache: Cache::default(),
            frame: 0,
            faces: Vec::new(),
            drawn: 0,
            records: Vec::new(),
            reach: LightReach::default(),
            edit_lights: HashMap::new(),
            near_edits: Vec::new(),
            stats: LocalShadowStats::default(),
        }
    }

    /// The faces this frame draws.
    pub fn faces(&self) -> &[Face] {
        &self.faces[..self.drawn]
    }

    /// Each light's shadow record, at its index.
    pub fn records(&self) -> &[LocalShadowRecord] {
        &self.records
    }

    /// Plans a frame of `scene` seen by a camera with `view` and
    /// `projection`, with visibility `mask` and its lights' shadows on when
    /// `enabled`.
    pub fn prepare(
        &mut self,
        drawn: &mut DrawInstances,
        scene: &Scene,
        (view, projection): (Mat4, Mat4),
        mask: u32,
        enabled: bool,
    ) {
        self.begin(scene);
        self.stats = LocalShadowStats::default();
        let volume = ViewVolume::new(projection * view);
        let candidates = candidates(scene, enabled, |light, shadow| {
            volume
                .reaches(light)
                .then(|| shadow.coverage(view, projection))
        });
        // The moving casters and their world bounds.
        let moving: Vec<_> = scene
            .instances
            .slots
            .iter()
            .filter(|(_, instance)| {
                instance.mobility == Mobility::Moving && instance.state.capture_visible
            })
            .map(|(id, instance)| {
                let model = scene.drawn_model(instance.state.model);
                let caster = MovingCaster {
                    instance: id,
                    pose: instance.state.pose,
                    model: instance.state.model,
                    geometry: model.geometry,
                    deformation: instance
                        .deformation
                        .as_ref()
                        .map_or(0, |deformation| deformation.revision),
                };
                let bounds = instance.bounds(model);
                (caster, bounds, posed_bounds(bounds, instance.state.pose))
            })
            .collect();
        for (id, shadow, coverage) in candidates {
            let Some(placement) = self
                .allocation
                .update(id, coverage, shadow.slots(), self.frame)
            else {
                self.stats.unshadowed += 1;
                continue;
            };
            self.stats.shadowed += 1;
            let layered = self.place(drawn, scene, Some(&moving), (id, shadow, placement), mask);
            self.records[id.index()] = record(shadow, placement, layered);
        }
    }

    /// Plans a probe capture at `center` of `scene` with visibility `mask`:
    /// the static layers of every casting light that is on, when
    /// `enabled`, placed by its coverage of the capture's faces, which the
    /// capture samples. A capture shows no moving instances, so it draws no
    /// frame faces, and it changes no frame's statistics.
    pub fn prepare_capture(
        &mut self,
        drawn: &mut DrawInstances,
        scene: &Scene,
        center: Vec3,
        mask: u32,
        enabled: bool,
    ) {
        self.begin(scene);
        let stats = self.stats;
        let candidates = candidates(scene, enabled, |_, shadow| {
            Some(shadow.capture_coverage(center))
        });
        for (id, shadow, coverage) in candidates {
            if let Some(placement) =
                self.allocation
                    .update(id, coverage, shadow.slots(), self.frame)
            {
                self.place(drawn, scene, None, (id, shadow, placement), mask);
                self.records[id.index()] = record(shadow, placement, true);
            }
        }
        self.stats = stats;
    }

    /// Starts planning a frame or capture of `scene`: no faces drawn yet,
    /// every light without a shadow, and the slots its pending static edits
    /// reach stale.
    fn begin(&mut self, scene: &Scene) {
        self.frame += 1;
        self.drawn = 0;
        let edits = &scene.static_edits;
        self.cache.begin(
            scene.id,
            atlas::cell_count(),
            edits.finished(),
            scene.origin(),
        );
        self.allocation
            .retain(|light| scene.lights.get(light).is_ok());
        self.mark_static_edits(scene);
        self.records.clear();
        self.records
            .resize(scene.lights.capacity(), LocalShadowRecord::NONE);
    }

    /// Marks the slots that the scene's pending static edits reach as stale:
    /// each edit's own bounds, tested first against the range of the light
    /// a slot shows and then against the slot's face, as Godot b130438
    /// pairs instances with the lights whose bounds they meet and dirties
    /// only the paired lights' shadows when one changes
    /// (servers/rendering/renderer_scene_cull.cpp, `_instance_pair` and
    /// `_update_instance`). A face no edit reaches keeps its layer however
    /// many edits there are elsewhere.
    fn mark_static_edits(&mut self, scene: &Scene) {
        let edits = scene.static_edits.pending();
        if edits.is_empty() {
            return;
        }
        // Each light's position and range, as its held slots show it, and
        // the edits within that range.
        let mut lights = std::mem::take(&mut self.edit_lights);
        let mut near = std::mem::take(&mut self.near_edits);
        lights.clear();
        near.clear();
        for (_, slot) in self.cache.held() {
            let key = slot
                .layer
                .or_else(|| slot.face.as_ref().map(|(key, _)| *key))
                .expect("held slots hold content");
            let reach = (key.view.position, key.view.range);
            let found = match lights.get(&key.light) {
                Some((at, found)) if *at == reach => found.clone(),
                _ => {
                    let start = near.len();
                    near.extend((0..edits.len()).filter(|&edit| within(edits[edit], reach)));
                    lights.insert(key.light, (reach, start..near.len()));
                    start..near.len()
                }
            };
            if found.is_empty() {
                continue;
            }
            let view = key.view.face(key.face).view_projection();
            if near[found]
                .iter()
                .any(|&edit| clip_intersects(edits[edit], view))
            {
                *slot = Slot::default();
            }
        }
        self.edit_lights = lights;
        self.near_edits = near;
    }

    /// Plans the faces of light `id`, seen as `shadow` and placed at
    /// `placement`, whose content changed, among the scene's `moving`
    /// casters with their models and world bounds; for a probe capture,
    /// which has none, only the faces' static layers. Returns whether its
    /// static layers hold its static casters once they are drawn: whether
    /// it stayed still.
    fn place(
        &mut self,
        drawn: &mut DrawInstances,
        scene: &Scene,
        moving: Option<&[Moving]>,
        (id, shadow, placement): (LightId, LightView, Placement),
        mask: u32,
    ) -> bool {
        let capture = moving.is_none();
        // A light seen differently in the last finished frame moved.
        let moved = !capture
            && self
                .cache
                .light(id)
                .is_some_and(|previous| *previous != shadow);
        if !capture {
            self.cache.shadowed(id, shadow);
        }
        let light = (shadow.position, shadow.range);
        // The moving casters its range reaches.
        let nearby: Vec<_> = moving
            .unwrap_or_default()
            .iter()
            .filter(|(_, _, bounds)| within(*bounds, light))
            .collect();
        // The static cluster groups its range reaches, found once for every
        // face that draws its static casters.
        let mut reach = std::mem::take(&mut self.reach);
        let mut reached = false;
        for face in (0..shadow.slots()).filter(|&face| shadow.draws(face)) {
            let slot = placement.cell(face);
            let key = FaceKey {
                light: id,
                view: shadow,
                face,
                mask,
                casters: scene.materials.casters,
            };
            // The face's moving casters: those whose bounds reach its view.
            let mut view = None;
            let moving: Vec<_> = nearby
                .iter()
                .filter(|(caster, bounds, _)| {
                    let view = view.get_or_insert_with(|| shadow.face(face));
                    moving_caster_reaches(*bounds, caster.pose, view.view_projection(), light)
                })
                .map(|(caster, _, _)| *caster)
                .collect();
            let held = self.cache.slot(slot);
            let work = if moved {
                Work::Full
            } else if held.layer != Some(key) {
                Work::Layer
            } else if !capture
                && held
                    .face
                    .as_ref()
                    .is_none_or(|(face, drawn)| *face != key || *drawn != moving)
            {
                Work::Moving
            } else {
                continue;
            };
            // A capture draws only the static layer, which leaves the frame
            // face as it was where it showed the same static casters.
            let content = if capture {
                held.face.clone().filter(|(face, _)| *face == key)
            } else {
                Some((key, moving))
            };
            self.cache.draw(
                slot,
                Slot {
                    layer: (work != Work::Full).then_some(key),
                    face: content,
                },
            );
            let view = view.unwrap_or_else(|| shadow.face(face));
            if work != Work::Moving && !reached {
                reach.find(scene, light);
                reached = true;
            }
            let face = self.face(work, placement.origin(face), placement.size, view);
            let population = |casters| Population::LocalShadow {
                position: light.0,
                range: light.1,
                casters,
                reach: &reach,
            };
            let mask = Some(mask);
            match work {
                Work::Full => {
                    face.casters
                        .build(drawn, scene, &view, mask, population(Casters::All))
                }
                Work::Layer => {
                    face.casters
                        .build(drawn, scene, &view, mask, population(Casters::Static))
                }
                Work::Moving => {}
            }
            if work != Work::Full && !capture {
                face.moving
                    .build(drawn, scene, &view, mask, population(Casters::Moving));
            }
            self.stats.layers_drawn += usize::from(work == Work::Layer);
            self.stats.faces_drawn += 1;
        }
        self.reach = reach;
        !moved
    }

    /// The next face this frame draws, reusing an earlier frame's lists.
    fn face(&mut self, work: Work, origin: [u32; 2], size: u32, view: View) -> &mut Face {
        if self.faces.len() == self.drawn {
            self.faces.push(Face {
                work,
                origin,
                size,
                view,
                casters: DrawList::default(),
                moving: DrawList::default(),
            });
        }
        let face = &mut self.faces[self.drawn];
        self.drawn += 1;
        face.work = work;
        face.origin = origin;
        face.size = size;
        face.view = view;
        face
    }

    /// Commits the last prepared frame, once submitted.
    pub fn finish(&mut self) {
        self.cache.finish();
    }
}

/// The casting lights of `scene` that are on, when `enabled`, with what
/// `coverage` gives them: their screen coverage, or none for a light it
/// leaves out; largest first, then in index order.
fn candidates(
    scene: &Scene,
    enabled: bool,
    coverage: impl Fn(&Light, &LightView) -> Option<f32>,
) -> Vec<(LightId, LightView, f32)> {
    let mut candidates: Vec<_> = scene
        .lights
        .on()
        .filter(|(_, light)| enabled && light.casts_shadow && light.range > LOCAL_SHADOW_NEAR)
        .filter_map(|(id, light)| {
            let shadow = LightView::new(light);
            coverage(light, &shadow).map(|coverage| (id, shadow, coverage))
        })
        .collect();
    candidates.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.index().cmp(&b.0.index())));
    candidates
}

/// The shadow record of a light seen as `shadow` at `placement`, whose
/// static layers hold its static casters when `layered`.
fn record(shadow: LightView, placement: Placement, layered: bool) -> LocalShadowRecord {
    let texel = 1. / placement.atlas as f32;
    // A face's width at a metre along its axis, over its texels: Bevy's
    // point and spot texel sizes (crates/bevy_pbr/src/render/light.rs).
    let (kind, width) = match shadow.shape {
        Shape::Cube { .. } => (LOCAL_SHADOW_CUBE, 2.),
        Shape::Spot { outer_angle, .. } => (LOCAL_SHADOW_SPOT, 2. * outer_angle.tan()),
    };
    let mut record = LocalShadowRecord {
        clip_from_world: Mat4::IDENTITY.to_cols_array_2d(),
        corners: [[0.; 4]; 3],
        size: placement.size as f32 * texel,
        near: LOCAL_SHADOW_NEAR,
        texel_scale: width / placement.size as f32,
        kind,
        layers: u32::from(layered),
        padding: [0; 3],
    };
    if let Shape::Spot { .. } = shadow.shape {
        record.clip_from_world = shadow.face(0).view_projection().to_cols_array_2d();
    }
    for face in 0..shadow.slots() {
        let [x, y] = placement.origin(face);
        record.corners[face / 2][face % 2 * 2..][..2]
            .copy_from_slice(&[x as f32 * texel, y as f32 * texel]);
    }
    record
}
