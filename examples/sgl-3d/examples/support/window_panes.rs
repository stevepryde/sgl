//! A window's light, for a spot light's cookie (`sgl_3d::LightCookie`):
//! generated in code, so the examples carry no image file and native and
//! browser builds share it.

/// The image's side in texels.
const SIZE: u32 = 256;
/// The window's half width and half height, in texels from the centre: its
/// corners lie inside the circle a spot's cone fills (half the side), so
/// the cone shows the whole window.
const HALF: [u32; 2] = [74, 94];
/// The frame's and the mullions' widths, in texels.
const FRAME: u32 = 10;
const MULLION: u32 = 6;
/// Panes across and down.
const PANES: [u32; 2] = [2, 3];

/// A square RGBA8 image of a window's light: two by three white panes
/// between dark mullions inside a dark frame, on black, opaque throughout,
/// its top the window's top. The panes pass the light whole and the rest
/// stops it.
pub fn window_panes() -> image::RgbaImage {
    image::RgbaImage::from_fn(SIZE, SIZE, |x, y| {
        let pane = [x, y]
            .iter()
            .zip(HALF)
            .zip(PANES)
            .all(|((&at, half), count)| {
                // Texels from the window's near edge along this axis.
                let Some(inside) = (at + half).checked_sub(SIZE / 2) else {
                    return false;
                };
                if inside < FRAME || inside >= 2 * half - FRAME {
                    return false;
                }
                // Across the glass between the frames, a mullion between panes.
                let glass = 2 * (half - FRAME);
                let along = inside - FRAME;
                (1..count).all(|bar| {
                    let centre = bar * glass / count;
                    along + MULLION / 2 < centre || along >= centre + MULLION / 2
                })
            });
        let value = if pane { 255 } else { 0 };
        image::Rgba([value, value, value, 255])
    })
}
