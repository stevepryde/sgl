//! SGL3D's scenario harness (`specs/testing.md`, "SGL3D scenario harness"),
//! test-support modules each scenario binary includes with `mod scenario;`:
//! scripted, deterministic multi-frame scenes rendered through the public
//! API on a real device and judged by numbers read from the frame's targets.
//!
//! - [`geometry`]: exact motion by frame index, the camera it gives, and the
//!   analytic truth of a fixture: projected positions, motion and coverage.
//! - [`image`]: decoded targets, regions of them and the metrics over a
//!   region.
//! - [`rig`] (native): a device, renderer, scene and output, fixtures of
//!   known radiance, readback and opt-in dumps.
//! - [`temporal`] (native): the temporal antialiasing suites' shared stage,
//!   scripts and oracles.
//!
//! A suite is a `tests/scenario_<feature>.rs` binary that includes these
//! modules. It builds fixtures with
//! [`rig::Shape`], drives them frame by frame from [`geometry::Motion`]s,
//! and asserts with one of the spec's oracles, named beside each assertion.
//! A later suite adds its fixtures to [`rig`] and its analytic truth to
//! [`geometry`].
// Each binary uses part of the harness.
#![allow(dead_code)]

pub mod geometry;
pub mod image;
#[cfg(not(target_arch = "wasm32"))]
pub mod rig;
#[cfg(not(target_arch = "wasm32"))]
pub mod temporal;
