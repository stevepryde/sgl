//! Reusable deterministic helpers, fixed-step timing, animation, and math.
//!
//! Dependency rule: this foundation crate has no internal SGL crate dependencies.

pub mod anim;
mod grid;
mod hash;
pub mod math;
pub mod prediction;
pub mod random;
mod rng;
pub mod time;

pub use grid::{Grid2, GridError};
pub use hash::{CanonicalWrite, Digest, StateHasher, digest_of};
pub use rng::{SplitMix64, derive_stream_seed};
