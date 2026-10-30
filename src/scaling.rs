//! The check of the tests that guard against a quadratic (or worse) cost:
//! that the work grows linearly with its input.
//!
//! A bound on the wall-clock time fails on a busy machine and lets a
//! regression pass on a fast one, so these tests compare two amounts of work
//! instead: the work at `factor × n` once, and the work at `n` repeated
//! `factor` times. For linear work the two are about equal; for quadratic work
//! the first is about `factor` times the second. The check passes while the
//! first is less than `√factor` times the second — halfway between, on a
//! logarithmic scale. Both sides take about as long, so a busy machine slows
//! both alike: a short run that fits in one time slice of the scheduler does
//! not get compared with a long one that does not.
//!
//! An attempt over the bound is repeated, up to three in all; one at the
//! quadratic ratio (`factor`) or above fails at once. [`assert_quadratic`]
//! does the same one power up, for work allowed to grow with the square of its
//! input: the work at `n` is repeated `factor²` times.
//!
//! This file is a unit-test module of the crate and is also included by the
//! integration tests (`tests/common/mod.rs`), so it uses nothing of the crate;
//! each of the two uses only some of it.

#![allow(dead_code)]

use std::time::{Duration, Instant};

/// How long `work` took.
pub fn timed(work: impl FnOnce()) -> Duration {
    let started = Instant::now();
    work();
    started.elapsed()
}

/// Panics unless the work `time` measures grows about linearly from `n` to
/// `factor × n`.
///
/// `time(size)` does the work at `size`, checks its result, and returns how
/// long the work itself took — setup such as building the input left out
/// ([`timed`]). The times are printed to the test's output.
pub fn assert_linear(what: &str, n: usize, factor: usize, time: impl FnMut(usize) -> Duration) {
    assert_growth(what, n, factor, 1, time);
}

/// [`assert_linear`] for work allowed to grow with the square of its input:
/// it guards against a cubic cost.
pub fn assert_quadratic(what: &str, n: usize, factor: usize, time: impl FnMut(usize) -> Duration) {
    assert_growth(what, n, factor, 2, time);
}

/// Panics unless the work at `factor × n` takes less than `√factor` times as
/// long as the work at `n` repeated `factor` to the `power` times.
fn assert_growth(
    what: &str,
    n: usize,
    factor: usize,
    power: u32,
    mut time: impl FnMut(usize) -> Duration,
) {
    let repeat = factor.pow(power);
    let bound = (factor as f64).sqrt();
    let mut slow = Vec::new();
    for _ in 0..3 {
        let small: Duration = (0..repeat).map(|_| time(n)).sum();
        let large = time(factor * n);
        let ratio = large.as_secs_f64() / small.as_secs_f64().max(1e-6);
        eprintln!(
            "{what}: {repeat} × {n} in {small:?}, {} in {large:?}: ×{ratio:.2}",
            factor * n
        );
        if ratio < bound {
            return;
        }
        slow.push(format!("{small:?} against {large:?} (×{ratio:.2})"));
        if ratio >= factor as f64 {
            break;
        }
    }
    panic!(
        "{what}: the work at {} took more than {bound:.2} times as long as the work at {n} \
         repeated {repeat} times: {}",
        factor * n,
        slow.join(", ")
    );
}
