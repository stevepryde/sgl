//! Original browser skies and exact Three PMREM atlases; see native-sky-export.html.
/// A caller-owned environment: the panorama the sky draws and its
/// prefiltered Three-compatible PMREM atlas, which lights surfaces and
/// reflections.
pub struct EnvironmentMap {
    pub panorama: image::RgbaImage,
    pub filtered: PmremAtlas,
}
pub struct PmremAtlas {
    pub width: u32,
    pub height: u32,
    pub rgba16: Vec<u8>,
}
