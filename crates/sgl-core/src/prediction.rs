//! Client prediction, server input queues and snapshot interpolation: the
//! bookkeeping of a networked game on a shared fixed step.
//!
//! The model is the one Gabriel Gambetta's "Fast-Paced Multiplayer" and
//! Glenn Fiedler's "Snapshot Interpolation" describe. A client predicts its
//! own body by stepping its inputs at once and keeping them until the
//! server acknowledges them; on each authoritative snapshot it takes the
//! server's state and replays the inputs the server has not yet run. The
//! server queues each client's inputs by tick and runs one a tick. Everything
//! the client does not predict is drawn a few ticks in the past, between two
//! snapshots.
//!
//! - [`Predictor`] holds the predicted state and the unacknowledged inputs,
//!   and reconciles them with the server's state.
//! - [`InputQueue`] is the server's per-client queue.
//! - [`Correction`] hides a reconcile's visual jump as a fading offset.
//! - [`SnapshotBuffer`] interpolates (and, briefly, extrapolates) snapshots
//!   of what the client does not predict.
//! - [`Sample`] is a body's previous and current state, for drawing between
//!   fixed steps at [`FixedClock::alpha`](crate::time::FixedClock::alpha).
//!
//! # Ticks
//!
//! A tick is a `u64` the game numbers. The input for tick `t` is applied by
//! the step that runs tick `t`, and the server acknowledges tick `t` by
//! sending the state after that step. A tick the server ran without the
//! client's input (it repeated the previous one) is acknowledged the same
//! way: the client's own input for it is dropped and the server's state
//! carries what really happened.
//!
//! # What stays the game's
//!
//! The game owns its loop, its connection and its messages, decides what is
//! predicted and who has authority, and chooses when to snap instead of
//! smoothing. It gives its step function and its interpolation function as
//! closures. Every function here is a pure function of its arguments: no
//! clocks, no randomness, the same on every target. Every buffer has a
//! capacity fixed at construction; a full one refuses and says so.
//! [`Correction`] and [`SnapshotBuffer`] are presentation: they never change
//! simulated state.

mod correction;
mod predictor;
mod queue;
mod sample;
mod snapshots;

pub use correction::Correction;
pub use predictor::{PredictError, Predictor, ReconcileError, Reconciled};
pub use queue::{InputQueue, Pushed, QueueFull, TickInput};
pub use sample::Sample;
pub use snapshots::{Playback, Sampled, SnapshotBuffer, SnapshotRejected};
