//! Lights: each point, spot and rectangle light's description and its
//! record in the scene's light buffer, which group 0 binds, with the `Scene`
//! operations that add, read, change and remove them. A removed light's
//! record stays until its index is reused; no view lists it.
use super::slots::Slots;
use super::{Scene, SceneError};
use crate::content::identity::{Identity, LightId};
use crate::content::light::{Light, LightShape};
use crate::shading::lights::LightRecord;

const RECORD: u64 = std::mem::size_of::<LightRecord>() as u64;

pub(crate) struct Lights {
    pub slots: Slots<LightId, Light>,
    buffer: wgpu::Buffer,
    /// How many of `slots` are rectangles.
    rects: usize,
}

impl Lights {
    /// Room for one record.
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            slots: Slots::default(),
            buffer: light_buffer(device, 1),
            rects: 0,
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

    fn write(&self, queue: &wgpu::Queue, index: usize, light: &Light) {
        queue.write_buffer(
            &self.buffer,
            index as u64 * RECORD,
            bytemuck::bytes_of(&LightRecord::new(light)),
        );
    }

    /// Room for `count` records, rewriting every record when the buffer is
    /// replaced; returns whether it was.
    fn reserve(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        count: usize,
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
            self.write(queue, id.index(), light);
        }
        Ok(true)
    }
}

fn light_buffer(device: &wgpu::Device, records: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("scene lights"),
        size: records * RECORD,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn is_rect(light: &Light) -> bool {
    matches!(light.shape, LightShape::Rect { .. })
}

/// A light a record can hold: finite, nonnegative colour, intensity,
/// specular scale and fog energy, a positive range, a spot's nonzero
/// direction and ordered angles below a right angle (Bevy's limit), and a
/// rectangle's nonzero direction, a width axis not parallel to it, and a
/// positive size.
fn validate(light: &Light) -> Result<(), SceneError> {
    let nonnegative = |value: f32| value.is_finite() && value >= 0.;
    let valid = light.position.is_finite()
        && light.color.iter().all(|&channel| nonnegative(channel))
        && nonnegative(light.intensity)
        && nonnegative(light.specular)
        && nonnegative(light.fog_energy)
        && light.range.is_finite()
        && light.range > 0.
        && match light.shape {
            LightShape::Point => true,
            LightShape::Spot {
                direction,
                inner_angle,
                outer_angle,
            } => {
                direction.is_finite()
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
        };
    if valid {
        Ok(())
    } else {
        Err(SceneError::InvalidLight)
    }
}

impl Scene {
    /// Adds a point, spot or rectangle light.
    pub fn add_light(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        light: Light,
    ) -> Result<LightId, SceneError> {
        validate(&light)?;
        let count = self.lights.slots.next_index() + 1;
        if self.lights.reserve(device, queue, count)? {
            self.resources = super::next_generation();
        }
        let id = self.lights.slots.insert(light);
        self.lights.rects += usize::from(is_rect(&light));
        self.lights.write(queue, id.index(), &light);
        Ok(id)
    }

    /// A light's current description.
    pub fn light(&self, id: LightId) -> Result<&Light, SceneError> {
        self.lights.get(id)
    }

    /// Replaces a light's description.
    pub fn set_light(
        &mut self,
        queue: &wgpu::Queue,
        id: LightId,
        light: Light,
    ) -> Result<(), SceneError> {
        let old = *self.lights.get(id)?;
        validate(&light)?;
        self.lights.rects =
            self.lights.rects + usize::from(is_rect(&light)) - usize::from(is_rect(&old));
        *self.lights.slots.get_mut(id).unwrap() = light;
        self.lights.write(queue, id.index(), &light);
        Ok(())
    }

    /// Removes a light. Its index is reused under a new identity.
    pub fn remove_light(&mut self, id: LightId) -> Result<(), SceneError> {
        let light = self
            .lights
            .slots
            .remove(id)
            .ok_or(SceneError::UnknownLight)?;
        self.lights.rects -= usize::from(is_rect(&light));
        Ok(())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
