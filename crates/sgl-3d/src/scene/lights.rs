//! Lights: each point, spot and rectangle light's description and its
//! record in the scene's light buffer, which group 0 binds, with the `Scene`
//! operations that add, read, change and remove them. A removed light's
//! record stays until its index is reused, or until the render origin moves
//! while a light of a higher index lives, which zeroes it; no view lists it.
//!
//! A spot's cookie is an image in the decal atlas (`decals`), and its
//! profile a grid there: a cookied or profiled light uses its image or
//! grid as a decal uses its images, so the atlas packs it, and its record
//! names that place in the layout the next upload binds, as a decal's
//! does; an upload that packs the atlas anew rewrites every cookied or
//! profiled light's record with the decals' (`Scene::upload_decals`).
use super::decal_atlas::{AtlasKey, Sampling};
use super::slots::Slots;
use super::{Scene, SceneError};
use crate::content::identity::{Identity, LightId};
use crate::content::light::{Light, LightShape};
use crate::shading::lights::{AtlasPlaces, LightRecord};

const RECORD: u64 = std::mem::size_of::<LightRecord>() as u64;

pub(crate) struct Lights {
    pub slots: Slots<LightId, Light>,
    buffer: wgpu::Buffer,
    /// How many of `slots` are rectangles.
    rects: usize,
    /// How many of `slots` project a cookie.
    cookies: usize,
    /// How many of `slots` shine with a profile.
    profiles: usize,
}

impl Lights {
    /// Room for one record.
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            slots: Slots::default(),
            buffer: light_buffer(device, 1),
            rects: 0,
            cookies: 0,
            profiles: 0,
        }
    }

    /// The records, as group 0 binds them.
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    /// The records the buffer holds: more than any light's index.
    pub fn capacity(&self) -> usize {
        (self.buffer.size() / RECORD) as usize
    }

    /// Whether the scene holds a rectangle light: the lit pipelines shade
    /// rectangles only while it does.
    pub fn holds_rect(&self) -> bool {
        self.rects > 0
    }

    /// Whether the scene holds a cookied light: the lit pipelines sample
    /// cookies only while it does.
    pub fn holds_cookie(&self) -> bool {
        self.cookies > 0
    }

    /// Whether the scene holds a profiled light: the lit pipelines and the
    /// fog evaluate profiles only while it does.
    pub fn holds_profile(&self) -> bool {
        self.profiles > 0
    }

    /// What the lights other than `except` use in the decal atlas, their
    /// cookies' images and their profiles, as the atlas packs them
    /// (`Scene::place_atlas`).
    pub fn atlas_keys(&self, except: Option<LightId>) -> impl Iterator<Item = AtlasKey> + '_ {
        self.slots
            .iter()
            .filter(move |(id, _)| Some(*id) != except)
            .flat_map(|(_, light)| atlas_keys(light))
    }

    pub fn get(&self, id: LightId) -> Result<&Light, SceneError> {
        self.slots.get(id).ok_or(SceneError::UnknownLight)
    }

    /// The lights that are on, with some intensity and colour, in index
    /// order: the ones every view lists and that may cast a shadow.
    pub fn on(&self) -> impl Iterator<Item = (LightId, &Light)> {
        self.slots
            .iter()
            .filter(|(_, light)| light.intensity > 0. && light.color.iter().any(|&c| c > 0.))
    }

    /// Writes `light`'s record at `index`, its cookie and profile at the
    /// places `places` gives (`atlas_places`).
    fn write(
        &self,
        queue: &wgpu::Queue,
        index: usize,
        light: &Light,
        places: &impl Fn(&Light) -> AtlasPlaces,
    ) {
        crate::counters::write_buffer(
            queue,
            &self.buffer,
            index as u64 * RECORD,
            bytemuck::bytes_of(&LightRecord::new(light, places(light))),
        );
    }

    /// Rewrites every cookied or profiled light's record, at the places
    /// `places` gives: after the atlas is packed anew.
    pub fn write_placed(&self, queue: &wgpu::Queue, places: &impl Fn(&Light) -> AtlasPlaces) {
        for (id, light) in self.slots.iter() {
            if light.cookie.is_some() || light.profile.is_some() {
                self.write(queue, id.index(), light, places);
            }
        }
    }

    /// Room for `count` records, rewriting every record when the buffer is
    /// replaced; returns whether it was.
    fn reserve(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        count: usize,
        places: &impl Fn(&Light) -> AtlasPlaces,
    ) -> Result<bool, SceneError> {
        let capacity = self.buffer.size() / RECORD;
        if count as u64 <= capacity {
            return Ok(false);
        }
        let limits = device.limits();
        let limit = limits
            .max_storage_buffer_binding_size
            .min(limits.max_buffer_size)
            / RECORD;
        if count as u64 > limit {
            return Err(SceneError::DeviceLimit);
        }
        self.buffer = light_buffer(device, (count as u64).max(capacity * 2).min(limit));
        for (id, light) in self.slots.iter() {
            self.write(queue, id.index(), light, places);
        }
        Ok(true)
    }
}

impl Lights {
    /// Moves the render origin by `by` (`Scene::move_origin`): each light's
    /// position, and every record in one write, as `Objects::write_all`
    /// writes the instances', rather than one write a light. The write
    /// zeroes the records of removed indices below the highest live one,
    /// which no view lists.
    pub fn move_origin(
        &mut self,
        queue: &wgpu::Queue,
        by: glam::Vec3,
        places: &impl Fn(&Light) -> AtlasPlaces,
    ) {
        let mut records = Vec::new();
        for (id, light) in self.slots.iter_mut() {
            light.position -= by;
            if records.len() <= id.index() {
                records.resize(id.index() + 1, bytemuck::Zeroable::zeroed());
            }
            records[id.index()] = LightRecord::new(light, places(light));
        }
        if !records.is_empty() {
            crate::counters::write_buffer(queue, &self.buffer, 0, bytemuck::cast_slice(&records));
        }
    }
}

fn light_buffer(device: &wgpu::Device, records: u64) -> wgpu::Buffer {
    crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some("scene lights"),
            size: records * RECORD,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        },
    )
}

fn is_rect(light: &Light) -> bool {
    matches!(light.shape, LightShape::Rect { .. })
}

/// The decal atlas's image `light` projects, sampled as colour.
fn cookie_key(light: &Light) -> Option<AtlasKey> {
    light
        .cookie
        .map(|cookie| AtlasKey::Image(cookie.image, Sampling::Color))
}

/// The profile `light` shines with, in the decal atlas.
fn profile_key(light: &Light) -> Option<AtlasKey> {
    light
        .profile
        .map(|profile| AtlasKey::Profile(profile.profile))
}

/// What `light` uses in the decal atlas: its cookie's image and its
/// profile.
fn atlas_keys(light: &Light) -> impl Iterator<Item = AtlasKey> {
    cookie_key(light).into_iter().chain(profile_key(light))
}

/// A cookie a record can hold: one with a frame (`cookie_frame`), on a
/// spot of a positive outer angle with an `up` that has a finite part
/// across the spot's direction.
fn valid_cookie(light: &Light) -> bool {
    light
        .cookie
        .is_none_or(|cookie| crate::shading::lights::cookie_frame(&cookie, &light.shape).is_some())
}

/// A profile a record can hold: one with a frame (`profile_zero`), on a
/// spot with a `zero_azimuth` that has a finite part across its direction.
fn valid_profile(light: &Light) -> bool {
    light.profile.is_none_or(|profile| {
        crate::shading::lights::profile_zero(&profile, &light.shape).is_some()
    })
}

/// Each light's cookie's and profile's places in the layout `decals`'
/// records name (`Decals::light_rect`), for the record writers.
pub(crate) fn atlas_places(decals: &super::decals::Decals) -> impl Fn(&Light) -> AtlasPlaces + '_ {
    move |light| AtlasPlaces {
        cookie: decals.light_rect(cookie_key(light)),
        profile: decals.light_rect(profile_key(light)),
        profile_size: light
            .profile
            .map_or([0; 2], |profile| decals.profile_size(profile.profile)),
    }
}

/// A light a record can hold: finite, nonnegative colour, intensity,
/// specular scale and fog energy, a shadow opacity in 0..=1, a positive
/// range, a spot's nonzero direction and ordered angles below a right angle
/// (Bevy's limit), a rectangle's nonzero direction, a width axis not
/// parallel to it, and a positive size, and a cookie and a profile only on
/// a spot (`valid_cookie`, `valid_profile`).
fn validate(light: &Light) -> Result<(), SceneError> {
    let nonnegative = |value: f32| value.is_finite() && value >= 0.;
    let valid = light.position.is_finite()
        && light.color.iter().all(|&channel| nonnegative(channel))
        && nonnegative(light.intensity)
        && nonnegative(light.specular)
        && nonnegative(light.fog_energy)
        && (0.0..=1.).contains(&light.shadow_opacity)
        && light.range.is_finite()
        && light.range > 0.
        && match light.shape {
            LightShape::Point { radius } => radius.is_finite() && radius >= 0.,
            LightShape::Spot {
                direction,
                inner_angle,
                outer_angle,
                radius,
            } => {
                radius.is_finite()
                    && radius >= 0.
                    && direction.is_finite()
                    && direction.length_squared() > 0.
                    && direction.normalize().is_finite()
                    && inner_angle >= 0.
                    && inner_angle <= outer_angle
                    && outer_angle < std::f32::consts::FRAC_PI_2
            }
            LightShape::Rect {
                direction,
                width_axis,
                width,
                height,
            } => {
                let normal = direction.normalize();
                normal.is_finite()
                    && width_axis.is_finite()
                    && width_axis
                        .reject_from_normalized(normal)
                        .normalize()
                        .is_finite()
                    && width.is_finite()
                    && width > 0.
                    && height.is_finite()
                    && height > 0.
                    && (light.intensity / (width * height)).is_finite()
            }
        }
        && valid_cookie(light)
        && valid_profile(light);
    if valid {
        Ok(())
    } else {
        Err(SceneError::InvalidLight)
    }
}

impl Scene {
    /// `light` is valid and its cookie's image and its profile are in the
    /// scene, and the decal atlas's layout that it needs, if the atlas
    /// lacks either, beside what the decals and the lights other than
    /// `replacing` use.
    fn light_layout(
        &self,
        light: &Light,
        replacing: Option<LightId>,
    ) -> Result<Option<super::decal_atlas::AtlasLayout>, SceneError> {
        validate(light)?;
        let adding: Vec<_> = atlas_keys(light).collect();
        self.decals.validate_images(&adding)?;
        self.place_atlas(&adding, None, replacing)
    }

    /// Counts `light` among the lights, once more or once less.
    fn count_light(&mut self, light: &Light, more: bool) {
        let count = |counter: &mut usize, counted: bool| {
            if counted && more {
                *counter += 1;
            } else if counted {
                *counter -= 1;
            }
        };
        count(&mut self.lights.rects, is_rect(light));
        count(&mut self.lights.cookies, light.cookie.is_some());
        count(&mut self.lights.profiles, light.profile.is_some());
        for key in atlas_keys(light) {
            self.decals.count_user(key, more);
        }
    }

    /// Adds a point, spot or rectangle light. One whose cookie or profile
    /// the decal atlas lacks packs the atlas anew before the next frame.
    pub fn add_light(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        light: Light,
    ) -> Result<LightId, SceneError> {
        self.edited();
        let layout = self.light_layout(&light, None)?;
        let count = self.lights.slots.next_index() + 1;
        let decals = &self.decals;
        if self
            .lights
            .reserve(device, queue, count, &atlas_places(decals))?
        {
            self.resources = super::next_generation();
        }
        self.decals.repack(layout);
        let id = self.lights.slots.insert(light);
        self.count_light(&light, true);
        let decals = &self.decals;
        self.lights
            .write(queue, id.index(), &light, &atlas_places(decals));
        Ok(id)
    }

    /// A light's current description.
    pub fn light(&self, id: LightId) -> Result<&Light, SceneError> {
        self.lights.get(id)
    }

    /// Replaces a light's description. One whose cookie or profile the
    /// decal atlas lacks packs the atlas anew before the next frame.
    pub fn set_light(
        &mut self,
        queue: &wgpu::Queue,
        id: LightId,
        light: Light,
    ) -> Result<(), SceneError> {
        let old = *self.lights.get(id)?;
        if old != light {
            self.edited();
        }
        let layout = self.light_layout(&light, Some(id))?;
        self.decals.repack(layout);
        self.count_light(&old, false);
        self.count_light(&light, true);
        *self.lights.slots.get_mut(id).unwrap() = light;
        let decals = &self.decals;
        self.lights
            .write(queue, id.index(), &light, &atlas_places(decals));
        Ok(())
    }

    /// Removes a light. Its index is reused under a new identity.
    pub fn remove_light(&mut self, id: LightId) -> Result<(), SceneError> {
        self.edited();
        let light = self
            .lights
            .slots
            .remove(id)
            .ok_or(SceneError::UnknownLight)?;
        self.count_light(&light, false);
        Ok(())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
