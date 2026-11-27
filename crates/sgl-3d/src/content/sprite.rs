//! Textured world-space sprites: a batch's look (`SpriteBatch`) and its
//! placements (`Sprite`), which a scene draws as one instanced draw per
//! batch, each sprite facing the view that draws it (the architecture's
//! Scene content, "Sprite batch"). The game simulates and places them; a
//! flipbook is the game advancing each sprite's `region`.
use crate::asset::Sampler;
use crate::content::identity::SpriteImageId;
use glam::{Vec2, Vec3};

/// A batch's look: the image its sprites show, how they face each view,
/// blend and size. A game sets its sprites apart (`Scene::set_sprites`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpriteBatch {
    /// The image (`Scene::add_sprite_image`), an atlas its sprites' regions
    /// cut cells from.
    pub image: SpriteImageId,
    /// How the image is filtered and wrapped: `asset::Sampler`'s nearest
    /// filters for pixel art. Sprites face the view, so anisotropic
    /// filtering does not apply to them.
    pub sampler: Sampler,
    pub facing: SpriteFacing,
    pub blend: SpriteBlend,
    pub sizing: SpriteSizing,
}

impl SpriteBatch {
    /// Sprites of `image`, sampled with glTF's default sampler, in the
    /// view's image plane, blended and sized in metres.
    pub fn new(image: SpriteImageId) -> Self {
        Self {
            image,
            sampler: Sampler::default(),
            facing: SpriteFacing::default(),
            blend: SpriteBlend::default(),
            sizing: SpriteSizing::default(),
        }
    }
}

/// How a batch's sprites face the view that draws them. Either way every
/// sprite of a batch lies in a plane parallel to the others', so the view
/// orders blended ones exactly.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum SpriteFacing {
    /// In the view's image plane, its up the view's up: three.js r187's
    /// `Sprite` and Godot's `BILLBOARD_ENABLED`. For hit markers, particles
    /// and anything that should face the screen.
    #[default]
    Camera,
    /// Upright along this world axis (any nonzero length) and turned about
    /// it to face the view's backward axis: Godot's `BILLBOARD_FIXED_Y`
    /// about any axis. For foliage cards and trees that stay upright.
    Axis(Vec3),
}

/// How a batch's sprites cover what lies behind them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum SpriteBlend {
    /// Blended by their alpha, back to front: for soft particles, smoke
    /// and hit markers. They write no depth.
    #[default]
    Blended,
    /// Additive color blending (`SrcAlpha` + `One`), writing no depth,
    /// ordered among blended surfaces: for fire embers, sparks, lightning
    /// and glowing magic particles.
    Additive,
    /// Opaque where their alpha reaches `cutoff` (0..=1), cut out
    /// elsewhere, writing depth and motion as opaque surfaces do: for
    /// foliage cards and pixel art. glTF's `alphaCutoff` default is 0.5.
    Masked { cutoff: f32 },
}

/// What a sprite's `size` measures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpriteSizing {
    /// Metres in the world: a sprite shrinks with distance.
    #[default]
    World,
    /// The size it has at one metre of view depth, kept at every depth, so
    /// it keeps its size on screen: three.js's `sizeAttenuation` off.
    Fixed,
}

/// One sprite of a batch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sprite {
    /// Where its pivot stands in the world.
    pub position: Vec3,
    /// Its width and height (`SpriteSizing`), finite and nonnegative.
    pub size: Vec2,
    /// The point of the quad at `position`: (0, 0) its bottom left, (1, 1)
    /// its top right, (0.5, 0.5) its middle (three.js's `center`). Finite;
    /// it may lie outside the quad.
    pub pivot: Vec2,
    /// Radians, anticlockwise as the view sees it, about the pivot.
    pub rotation: f32,
    /// The part of the image it shows.
    pub region: SpriteRegion,
    /// Linear RGB its texels are multiplied by (above 1 to glow) and alpha
    /// (0..=1) their alpha is.
    pub tint: [f32; 4],
}

impl Sprite {
    /// `size` at `position`, pivoted on its middle, upright, showing the
    /// whole image untinted.
    pub fn new(position: Vec3, size: Vec2) -> Self {
        Self {
            position,
            size,
            pivot: Vec2::splat(0.5),
            rotation: 0.,
            region: SpriteRegion::default(),
            tint: [1.; 4],
        }
    }
}

impl Default for Sprite {
    /// A 1 m sprite at the origin.
    fn default() -> Self {
        Self::new(Vec3::ZERO, Vec2::ONE)
    }
}

/// A rectangle of an image in texture coordinates, u to the right and v
/// down from the image's top left: `min` shows at the sprite's top left
/// and `max` at its bottom right. Swap a pair to mirror it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpriteRegion {
    pub min: [f32; 2],
    pub max: [f32; 2],
}

impl Default for SpriteRegion {
    /// The whole image.
    fn default() -> Self {
        Self {
            min: [0.; 2],
            max: [1.; 2],
        }
    }
}

impl SpriteRegion {
    /// Cell `index` of an image cut into `columns` by `rows` equal cells,
    /// counted along each row from the top left: a flipbook's frame or an
    /// atlas's entry. An index past the last cell wraps.
    pub fn cell(columns: u32, rows: u32, index: u32) -> Self {
        let (columns, rows) = (columns.max(1), rows.max(1));
        let index = index % (columns * rows);
        let (column, row) = (index % columns, index / columns);
        let size = [1. / columns as f32, 1. / rows as f32];
        Self {
            min: [column as f32 * size[0], row as f32 * size[1]],
            max: [(column + 1) as f32 * size[0], (row + 1) as f32 * size[1]],
        }
    }
}

/// Whether `batch`'s values are ones a scene draws.
pub(crate) fn batch_valid(batch: &SpriteBatch) -> bool {
    let facing = match batch.facing {
        SpriteFacing::Camera => true,
        SpriteFacing::Axis(axis) => axis.is_finite() && axis.length_squared() > 0.,
    };
    let blend = match batch.blend {
        SpriteBlend::Blended | SpriteBlend::Additive => true,
        SpriteBlend::Masked { cutoff } => (0.0..=1.).contains(&cutoff),
    };
    facing && blend
}

/// Whether `sprite`'s values are ones a scene draws.
pub(crate) fn sprite_valid(sprite: &Sprite) -> bool {
    sprite.position.is_finite()
        && sprite.size.is_finite()
        && sprite.size.min_element() >= 0.
        && sprite.pivot.is_finite()
        && sprite.rotation.is_finite()
        && sprite
            .region
            .min
            .iter()
            .chain(&sprite.region.max)
            .all(|value| value.is_finite())
        && sprite.tint[..3]
            .iter()
            .all(|value| value.is_finite() && *value >= 0.)
        && (0.0..=1.).contains(&sprite.tint[3])
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// A 4 by 2 sheet's cells, by hand: the sixth is the second of the
    /// second row, an index past the eighth wraps to the first.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_cell_is_the_grid_rectangle_counted_from_the_top_left() {
        assert_eq!(
            SpriteRegion::cell(4, 2, 5),
            SpriteRegion {
                min: [0.25, 0.5],
                max: [0.5, 1.]
            }
        );
        assert_eq!(SpriteRegion::cell(4, 2, 8), SpriteRegion::cell(4, 2, 0));
        assert_eq!(
            SpriteRegion::cell(4, 2, 0),
            SpriteRegion {
                min: [0., 0.],
                max: [0.25, 0.5]
            }
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn sprites_outside_the_documented_ranges_are_refused() {
        let ok = Sprite::default();
        assert!(sprite_valid(&ok));
        for bad in [
            Sprite {
                size: Vec2::new(-1., 1.),
                ..ok
            },
            Sprite {
                tint: [1., 1., 1., 1.5],
                ..ok
            },
            Sprite {
                tint: [-0.1, 1., 1., 1.],
                ..ok
            },
            Sprite {
                position: Vec3::new(f32::NAN, 0., 0.),
                ..ok
            },
            Sprite {
                region: SpriteRegion {
                    min: [0., f32::INFINITY],
                    max: [1., 1.],
                },
                ..ok
            },
        ] {
            assert!(!sprite_valid(&bad), "{bad:?}");
        }
    }
}
