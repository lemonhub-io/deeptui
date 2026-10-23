//! Concurrency derived from declared effects, not from a list of names.
//!
//! A model reply may hold several tool calls. Reads commute with each other;
//! anything that changes the world does not. So the calls are cut into
//! *waves*: a run of consecutive reads is one wave that executes concurrently,
//! and every write is a wave of its own. Waves run in order, which makes the
//! batch observe exactly what serial execution would — at the latency of its
//! slowest read instead of the sum of them.

use std::ops::Range;

/// What a tool does to the world. The only thing the scheduler needs to know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Observes the workspace; commutes with every other read.
    Read,
    /// Changes the workspace or runs arbitrary code: a barrier.
    Write,
}

/// Index ranges of the calls, in order: each range is one wave.
pub fn waves(effects: impl IntoIterator<Item = Effect>) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut prev = Effect::Write;
    for (i, e) in effects.into_iter().enumerate() {
        match (prev, e) {
            (Effect::Read, Effect::Read) => out.last_mut().expect("a read opened it").end = i + 1,
            _ => out.push(i..i + 1),
        }
        prev = e;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::Effect::{Read as R, Write as W};
    use super::*;

    #[test]
    #[allow(clippy::single_range_in_vec_init)] // one wave is the point
    fn reads_share_a_wave_and_writes_stand_alone() {
        assert_eq!(waves([]), Vec::<Range<usize>>::new());
        assert_eq!(waves([R]), [0..1]);
        assert_eq!(waves([R, R, R]), [0..3]);
        assert_eq!(waves([W, W]), [0..1, 1..2]);
        // a write is a barrier: reads on either side never cross it
        assert_eq!(waves([R, R, W, R, R]), [0..2, 2..3, 3..5]);
        assert_eq!(waves([W, R, R]), [0..1, 1..3]);
        assert_eq!(waves([R, W]), [0..1, 1..2]);
    }

    #[test]
    fn waves_cover_every_call_once_in_order() {
        let effects = [R, W, R, R, W, W, R];
        let ws = waves(effects);
        let flat: Vec<usize> = ws.iter().cloned().flatten().collect();
        assert_eq!(flat, (0..effects.len()).collect::<Vec<_>>());
        assert!(ws.iter().all(|w| !w.is_empty()));
    }
}
