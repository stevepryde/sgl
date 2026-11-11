//! Effective generation state. Requested settings remain unchanged on fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameGenerationStatus {
    /// Frame generation was not requested.
    Off,
    /// FSR3 generation is available for the current frame.
    Active,
    /// The frame uses real images only; the reason identifies the unavailable boundary.
    Unavailable(String),
}
