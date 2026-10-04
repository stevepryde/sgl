//! Shared runner for the crate's native-only property tests: a fixed seed
//! (testing.md 3), no failure-persistence files, and `PROPTEST_CASES` to
//! widen a local run.

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};

const SEED: [u8; 32] = *b"sgl-net property tests seed   01";

/// Run `test` over `strategy`; panics with proptest's minimal failing case.
pub(crate) fn check<S: Strategy>(
    strategy: S,
    test: impl Fn(S::Value) -> Result<(), TestCaseError>,
) {
    let config = Config {
        failure_persistence: None,
        ..Config::default()
    };
    let mut runner =
        TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &SEED));
    if let Err(failure) = runner.run(&strategy, test) {
        panic!("{failure}");
    }
}

/// Arbitrary bytes up to `max` long.
pub(crate) fn bytes(max: usize) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=max)
}
