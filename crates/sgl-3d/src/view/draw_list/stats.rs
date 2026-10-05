//! The camera's submitted geometry, which `Renderer::geometry_stats`
//! reports.
use crate::Mobility;
use std::ops::Range;

/// The camera's submitted geometry by instance mobility, after visibility,
/// culling and LOD selection and before GPU backface culling: (draws,
/// triangles). Its opaque and masked surfaces are GPU-built, counted on the
/// GPU, one section of at most 128 triangles a draw; its blended ones are
/// CPU-built, one instanced draw a draw, which holds instances of one
/// mobility and submits its triangles once per instance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GeometryStats {
    pub static_instances: (usize, u64),
    pub moving_instances: (usize, u64),
}

impl GeometryStats {
    /// These draws and `other`'s.
    pub(crate) fn with(self, other: &Self) -> Self {
        let sum = |a: (usize, u64), b: (usize, u64)| (a.0 + b.0, a.1 + b.1);
        Self {
            static_instances: sum(self.static_instances, other.static_instances),
            moving_instances: sum(self.moving_instances, other.moving_instances),
        }
    }

    /// Draw calls and triangles of all content.
    pub fn total(&self) -> (usize, u64) {
        (
            self.static_instances.0 + self.moving_instances.0,
            self.static_instances.1 + self.moving_instances.1,
        )
    }

    /// A draw of `range` for `instances` instances of `mobility`.
    pub(super) fn add(&mut self, mobility: Mobility, range: &Range<u32>, instances: u64) {
        let total = match mobility {
            Mobility::Static => &mut self.static_instances,
            Mobility::Moving => &mut self.moving_instances,
        };
        total.0 += 1;
        total.1 += u64::from((range.end - range.start) / 3) * instances;
    }
}
