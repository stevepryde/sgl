//! Instance footprints (the register's M-23): where an instance changed by
//! an edit no motion vector describes stood in the frames the next one may
//! follow, and where it stands, which every view whose frame has the
//! temporal masks marks in its transparency and composition mask until a
//! frame begun after the edit is finished (`view::footprints`,
//! `stages::transparent::footprints`); the records the stood states draw
//! from, bound as group 1 in place of the object records over the same
//! scene source; and the scene-source words a removed or replaced model,
//! or a removed deforming instance, held, kept while a footprint may name
//! them. A change footprints cannot mark raises the view-wide signal
//! instead (M-9): one past their cap, or one of an instance drawn with a
//! shader that displaces its vertices and is not marked every frame.
use super::Scene;
use super::instances::Drawn;
use super::materials::Material;
use super::origin::translated;
use super::rays::SceneRays;
use super::static_edits::posed_bounds;
use crate::content::identity::{InstanceId, ModelId};
use crate::shading::uniforms::ObjectUniform;
use glam::{Mat4, Vec3};
use std::ops::Range;

/// The most footprint draws pending at once, one for each marked mesh of
/// each stood state and stand, which bounds the draws in a view that sees
/// them all and the stood states' records. A change that would pass it
/// raises the view-wide signal.
pub(crate) const MAX_FOOTPRINT_DRAWS: usize = 1024;
/// The most triangles the pending footprints draw at their base level in a
/// view that sees them all, which bounds the marking pass's vertex work: a
/// pass drawing 1.3 million took 0.17 ms on an Apple M-series GPU at
/// 1280x720 (#892). A change that would pass it raises the view-wide
/// signal.
pub(crate) const MAX_FOOTPRINT_TRIANGLES: u64 = 1 << 21;

/// One mesh of a stood state: its record in the scene source, its
/// triangles and its material's visibility group as the frame drew it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FootprintMesh {
    pub word: u32,
    pub triangles: u32,
    pub group: u32,
}

/// Where an instance stood: its opaque and masked meshes as a frame drew
/// them, at their base level.
pub(crate) struct Stood {
    pub pose: Mat4,
    /// Its world bounds there.
    pub bounds: [Vec3; 2],
    /// Its deformed positions' slot in the scene source; zero for a rigid
    /// instance.
    pub positions: u32,
    pub meshes: Vec<FootprintMesh>,
}

impl Stood {
    /// Its footprint record: an object record read for its pose and
    /// deformed positions alone.
    fn record(&self) -> ObjectUniform {
        let pose = self.pose.to_cols_array_2d();
        ObjectUniform {
            model: pose,
            previous_model: pose,
            baked_irradiance: [[0.; 4]; 6],
            flags: 0,
            deformed_positions: self.positions,
            previous_positions: self.positions,
            deformed_normals: 0,
        }
    }

    fn triangles(&self) -> u64 {
        self.meshes
            .iter()
            .map(|mesh| u64::from(mesh.triangles))
            .sum()
    }
}

/// A changed instance, marked where it stands from its object record, and
/// the triangles its model's marked meshes had when it was made.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Stand {
    pub id: InstanceId,
    draws: usize,
    triangles: u64,
}

/// The stood states' records, kept between frames for their capacity, and
/// the group binding them while a footprint is pending: it holds the scene
/// source, which a growth replaces, so it goes with the last footprint.
struct Records {
    buffer: wgpu::Buffer,
    /// The group and the buffers it binds: the records, the scene source,
    /// its instances and the instances' shader data.
    group: Option<(wgpu::BindGroup, [wgpu::Buffer; 4])>,
}

/// The scene's pending footprints. Each entry carries whether a frame
/// begun since it was made rendered it, which retires it when that frame
/// is finished, as `Sprites::departed` does.
#[derive(Default)]
pub(crate) struct Footprints {
    /// Where changed instances stood, each drawn from the record at its
    /// index.
    pub stood: Vec<(Stood, bool)>,
    /// Where changed instances stand.
    pub stands: Vec<(Stand, bool)>,
    /// Scene-source words freed once no footprint can name them.
    retiring: Vec<(Range<u32>, bool)>,
    /// The pending footprints' draws and triangles.
    draws: usize,
    triangles: u64,
    records: Option<Records>,
}

impl Footprints {
    /// Whether no footprint is pending: a frame then draws none.
    pub fn is_empty(&self) -> bool {
        self.stood.is_empty() && self.stands.is_empty()
    }

    /// Whether `draws` more footprint draws of `triangles` more triangles
    /// fit.
    fn fit(&self, draws: usize, triangles: u64) -> bool {
        self.draws + draws <= MAX_FOOTPRINT_DRAWS
            && self.triangles + triangles <= MAX_FOOTPRINT_TRIANGLES
    }

    /// The group binding the stood states' records in place of the object
    /// records, as the frame being rendered wrote them.
    pub fn group(&self) -> Option<&wgpu::BindGroup> {
        let records = self.records.as_ref()?;
        records.group.as_ref().map(|(group, _)| group)
    }

    /// Frees `range` of the scene source now, or once no footprint can
    /// name it: while one is pending, when a frame begun after this is
    /// finished.
    pub fn free(&mut self, rays: &mut SceneRays, range: Range<u32>) {
        if self.stood.is_empty() {
            rays.free(range);
        } else {
            self.retiring.push((range, false));
        }
    }

    /// Moves the render origin by `by` (`Scene::move_origin`): each stood
    /// state's pose and bounds, whose record the next frame writes.
    pub fn move_origin(&mut self, by: Vec3) {
        for (stood, _) in &mut self.stood {
            stood.pose = translated(stood.pose, by);
            stood.bounds = stood.bounds.map(|corner| corner - by);
        }
    }

    /// Begins a frame: each footprint and deferred free is the frame's,
    /// and the stood states' records are written, bound over `layout` with
    /// the scene's `source`, `instances` and `shader_data`.
    pub fn begin_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        [source, instances, shader_data]: [&wgpu::Buffer; 3],
    ) {
        for (_, rendered) in &mut self.stood {
            *rendered = true;
        }
        for (_, rendered) in &mut self.stands {
            *rendered = true;
        }
        for (_, rendered) in &mut self.retiring {
            *rendered = true;
        }
        if self.stood.is_empty() {
            return;
        }
        let records: Vec<ObjectUniform> = self.stood.iter().map(|(s, _)| s.record()).collect();
        let bytes: &[u8] = bytemuck::cast_slice(&records);
        let held = self.records.as_ref().map_or(0, |r| r.buffer.size());
        let buffer = match &self.records {
            Some(records) if held >= bytes.len() as u64 => records.buffer.clone(),
            // Grown geometrically, as the object records are.
            _ => crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("instance footprint records"),
                    size: (bytes.len() as u64).max(held.saturating_mul(2)),
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
        };
        let bound = [
            buffer.clone(),
            source.clone(),
            instances.clone(),
            shader_data.clone(),
        ];
        let group = match self.records.take().and_then(|records| records.group) {
            Some((group, held)) if held == bound => (group, held),
            _ => (super::scene_group(device, layout, &bound), bound),
        };
        self.records = Some(Records {
            buffer: buffer.clone(),
            group: Some(group),
        });
        crate::counters::write_buffer(queue, &buffer, 0, bytes);
    }

    /// Finishes the frame last begun: the footprints and deferred frees it
    /// rendered retire, freeing their words of `rays`.
    pub fn finish_frame(&mut self, rays: &mut SceneRays) {
        if self.is_empty() && self.retiring.is_empty() {
            return;
        }
        self.stood.retain(|(_, rendered)| !rendered);
        self.stands.retain(|(_, rendered)| !rendered);
        self.retiring.retain(|(range, rendered)| {
            if *rendered {
                rays.free(range.clone());
            }
            !rendered
        });
        self.draws = self
            .stood
            .iter()
            .map(|(s, _)| s.meshes.len())
            .sum::<usize>()
            + self.stands.iter().map(|(s, _)| s.draws).sum::<usize>();
        self.triangles = self.stood.iter().map(|(s, _)| s.triangles()).sum::<u64>()
            + self.stands.iter().map(|(s, _)| s.triangles).sum::<u64>();
        if self.stood.is_empty()
            && let Some(records) = &mut self.records
        {
            records.group = None;
        }
    }
}

/// What a mesh drawn with a material needs, where it was or is drawn,
/// after a change no motion vector describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Need {
    /// Its footprint: an opaque or masked mesh that stands where its
    /// triangles lie.
    Footprint,
    /// Nothing: a blended or transmissive mesh, whose coverage every frame
    /// that drew it marked (M-11), or one whose material is marked every
    /// frame it is drawn (its normal layers move or its shader reads the
    /// time: M-16).
    Nothing,
    /// The view-wide signal: its material's shader displaces its vertices
    /// where a footprint drawn without the shader would not mark them.
    ViewWide,
}

impl Scene {
    /// What a mesh drawn with `material` needs (`Need`).
    pub(crate) fn need(&self, material: &Material) -> Need {
        let reads_time = material.values.shader.is_some_and(|shader| {
            self.shaders
                .get(shader.shader)
                .is_ok_and(|shader| shader.reads_time)
        });
        if material.values.blended() || reads_time || material.record_moves() {
            Need::Nothing
        } else if material.displacement_bound() > 0. {
            Need::ViewWide
        } else {
            Need::Footprint
        }
    }

    /// Where `drawn` stood: its model's meshes that need a footprint, as
    /// their materials are now. None where one needs the view-wide signal.
    fn stood(&self, drawn: &Drawn, overrides: Option<&[crate::MaterialId]>) -> Option<Stood> {
        let model = self.drawn_model(drawn.model);
        let mut meshes = Vec::new();
        for (index, mesh) in model.meshes.iter().enumerate() {
            let material_id = overrides
                .and_then(|mats| mats.get(index).copied())
                .unwrap_or(mesh.material);
            let material = self.drawn_material(material_id);
            match self.need(material) {
                Need::ViewWide => return None,
                Need::Nothing => {}
                Need::Footprint if mesh.count == 0 => {}
                Need::Footprint => meshes.push(FootprintMesh {
                    word: model.ray.mesh_word(index),
                    triangles: mesh.count / 3,
                    group: material.values.visibility_group,
                }),
            }
        }
        Some(Stood {
            pose: drawn.pose,
            bounds: posed_bounds(drawn.bounds.unwrap_or(model.bounds), drawn.pose),
            positions: drawn.positions,
            meshes,
        })
    }

    /// Marks where instance `id` stood (M-23), before an edit no motion
    /// vector describes: each state the frames the next one may follow
    /// drew of it, drawn with `model` where one is given, that its
    /// footprints do not hold yet. Where one cannot be marked, past the cap
    /// or under a displacing shader, raises the view-wide signal (M-9).
    pub(crate) fn footprint_stood(&mut self, id: InstanceId, model: Option<ModelId>) {
        let Some(instance) = self.instances.slots.get(id) else {
            return;
        };
        let overrides = instance
            .material_override_meshes()
            .map(<[crate::MaterialId]>::to_vec);
        let (states, held) = instance.uncaptured(&self.models, |drawn| {
            model.is_none_or(|model| model == drawn)
        });
        let mut stood = Vec::new();
        let mut view_wide = false;
        for drawn in states.iter().flatten() {
            let mats = overrides
                .as_deref()
                .filter(|_| drawn.model == instance.state.model);
            match self.stood(drawn, mats) {
                Some(state) if state.meshes.is_empty() => {}
                Some(state) => stood.push(state),
                None => view_wide = true,
            }
        }
        let triangles: u64 = stood.iter().map(Stood::triangles).sum();
        let draws: usize = stood.iter().map(|state| state.meshes.len()).sum();
        let footprints = &mut self.footprints;
        if view_wide || !footprints.fit(draws, triangles) {
            self.instances.raise_view_wide();
        } else {
            footprints.draws += draws;
            footprints.triangles += triangles;
            footprints
                .stood
                .extend(stood.into_iter().map(|state| (state, false)));
        }
        self.instances
            .slots
            .get_mut(id)
            .expect("a live instance")
            .captured(held);
    }

    /// Marks where instance `id` stands (M-23), after an edit no motion
    /// vector describes: once a frame, where it is visible, from its object
    /// record. Where it cannot be marked, past the cap or under a
    /// displacing shader, raises the view-wide signal (M-9).
    pub(crate) fn footprint_stands(&mut self, id: InstanceId) {
        let Some(instance) = self.instances.slots.get(id) else {
            return;
        };
        if !instance.state.visible || !instance.stands_uncaptured() {
            return;
        }
        let model = self.drawn_model(instance.state.model);
        let (mut draws, mut triangles) = (0, 0);
        let mut view_wide = false;
        for (index, mesh) in model.meshes.iter().enumerate() {
            match self.need(self.drawn_material(instance.mesh_material(index, mesh.material))) {
                Need::Footprint if mesh.count > 0 => {
                    draws += 1;
                    triangles += u64::from(mesh.count / 3);
                }
                Need::Footprint | Need::Nothing => {}
                Need::ViewWide => view_wide = true,
            }
        }
        let footprints = &mut self.footprints;
        if view_wide || !footprints.fit(draws, triangles) {
            self.instances.raise_view_wide();
        } else if draws > 0 {
            footprints.draws += draws;
            footprints.triangles += triangles;
            footprints.stands.push((
                Stand {
                    id,
                    draws,
                    triangles,
                },
                false,
            ));
        } else {
            // Nothing of it needs marking now; an edit later in the window
            // may change that (a material made to need a footprint).
            return;
        }
        self.instances
            .slots
            .get_mut(id)
            .expect("a live instance")
            .stands_captured();
    }

    /// Marks where the instances that showed or show `model` stood, before
    /// an edit of its geometry or of what a material draws on it.
    pub(crate) fn footprint_model_stood(&mut self, model: ModelId) -> Vec<InstanceId> {
        let showing = self.instances.showing(model);
        for &id in &showing {
            self.footprint_stood(id, Some(model));
        }
        showing
    }

    /// Marks where `showing`, the instances `footprint_model_stood` found,
    /// stand after the edit, those that still show `model`.
    pub(crate) fn footprint_model_stands(&mut self, model: ModelId, showing: &[InstanceId]) {
        for &id in showing {
            if self
                .instances
                .slots
                .get(id)
                .is_some_and(|instance| instance.state.model == model)
            {
                self.footprint_stands(id);
            }
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
impl Footprints {
    /// How many stood states and stands are pending.
    pub fn pending(&self) -> [usize; 2] {
        [self.stood.len(), self.stands.len()]
    }

    /// How many scene-source ranges wait to be freed.
    pub fn retiring(&self) -> usize {
        self.retiring.len()
    }
}
