//! The engine's time as wall-clock time. Inside, everything runs on the
//! engine's own clock (sample counts, from 0), which never jumps; outside,
//! times are Unix: a call's start and stop, its transmissions, unit events.
//! The wall clock at the engine's time 0 (the epoch) is set at Start, and
//! moved while recording as the host's wall clock is corrected — a time
//! service slewing or stepping it, or a monotonic clock that runs at its
//! crystal's rate rather than the corrected one (macOS's).
//!
//! A call is converted by one epoch from start to stop, the one in effect
//! when it began, so its length and its transmissions' places in it hold
//! however the epoch moves meanwhile, and what was said of it while it was
//! up (its start time) is what its record says. That's why the epochs are
//! kept as a history, not only the latest.

use std::collections::VecDeque;

/// A move of the epoch smaller than this is ignored, ms: times are kept to
/// the ms, and the host's reading of its clock jitters by less.
pub const MIN_MOVE_MS: f64 = 1.0;
/// Epochs are kept this long after they were replaced, engine s: for the
/// calls still up that began under them. A call that began longer ago than
/// this is converted by the oldest kept.
pub const KEEP_S: f64 = 4.0 * 3600.0;
/// And no more than this many (the oldest go first).
pub const KEEP_MAX: usize = 8192;

/// The wall clock (Unix ms) at the engine's time 0, as it has moved.
#[derive(Clone, Debug)]
pub struct WallEpoch {
    /// (from engine time s, epoch ms), in order; the first applies from the start.
    epochs: VecDeque<(f64, f64)>,
}

impl WallEpoch {
    pub fn new(epoch_ms: f64) -> Self {
        WallEpoch { epochs: VecDeque::from([(f64::NEG_INFINITY, epoch_ms)]) }
    }

    /// The epoch now (the latest set).
    pub fn now(&self) -> f64 {
        self.epochs.back().map_or(0.0, |e| e.1)
    }

    /// The epoch in effect at engine time `t_s`.
    pub fn at(&self, t_s: f64) -> f64 {
        let i = self.epochs.partition_point(|e| e.0 <= t_s);
        self.epochs.get(i.saturating_sub(1)).map_or(0.0, |e| e.1)
    }

    /// Engine time `t_s` as Unix ms, by the epoch in effect at `anchor_s`
    /// (a call's start, for each of its times).
    pub fn ms(&self, t_s: f64, anchor_s: f64) -> f64 {
        self.at(anchor_s) + t_s * 1000.0
    }

    /// The epoch is `epoch_ms` from engine time `now_s` on. False when it
    /// moved by less than [`MIN_MOVE_MS`] (or not at all), and so wasn't.
    pub fn set(&mut self, now_s: f64, epoch_ms: f64) -> bool {
        if !epoch_ms.is_finite() || !now_s.is_finite() || (epoch_ms - self.now()).abs() < MIN_MOVE_MS {
            return false;
        }
        match self.epochs.back_mut() {
            // Moved again before the engine's time did: one change, not two.
            Some(last) if last.0 >= now_s => last.1 = epoch_ms,
            _ => self.epochs.push_back((now_s, epoch_ms)),
        }
        // The one in effect KEEP_S ago is the oldest needed.
        while self.epochs.len() > KEEP_MAX || self.epochs.get(1).is_some_and(|e| e.0 <= now_s - KEEP_S) {
            self.epochs.pop_front();
        }
        true
    }

    /// Epochs kept.
    pub fn len(&self) -> usize {
        self.epochs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.epochs.is_empty()
    }
}

impl Default for WallEpoch {
    fn default() -> Self {
        WallEpoch::new(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmoved_it_is_the_start_epoch_everywhere() {
        let w = WallEpoch::new(1_700_000_000_000.0);
        for t in [-5.0, 0.0, 10.0, 1e7] {
            assert_eq!(w.at(t), 1_700_000_000_000.0);
        }
        assert_eq!(w.ms(2.5, 2.5), 1_700_000_002_500.0);
    }

    #[test]
    fn each_time_by_the_epoch_of_its_anchor() {
        let mut w = WallEpoch::new(1000.0);
        assert!(w.set(10.0, 1500.0));
        assert!(w.set(20.0, 900.0));
        assert_eq!((w.at(9.999), w.at(10.0), w.at(19.0), w.at(20.0), w.at(99.0)), (1000.0, 1500.0, 1500.0, 900.0, 900.0));
        assert_eq!(w.now(), 900.0);
        // A call begun at 5 s and stopped at 25 s keeps its first epoch: 20 s long, whatever moved.
        let (start, stop) = (w.ms(5.0, 5.0), w.ms(25.0, 5.0));
        assert_eq!(stop - start, 20_000.0);
        assert_eq!(start, 1000.0 + 5000.0);
    }

    #[test]
    fn small_moves_and_nonsense_are_ignored() {
        let mut w = WallEpoch::new(1000.0);
        assert!(!w.set(1.0, 1000.0));
        assert!(!w.set(1.0, 1000.0 + MIN_MOVE_MS * 0.9));
        assert!(!w.set(1.0, f64::NAN));
        assert!(!w.set(f64::NAN, 2000.0));
        assert!(!w.set(1.0, f64::INFINITY));
        assert_eq!((w.len(), w.now()), (1, 1000.0));
        // Small moves don't add up behind its back: it's compared with the epoch kept.
        for i in 1..=4 {
            assert!(!w.set(i as f64, 1000.0 + 0.2 * i as f64));
        }
        assert_eq!(w.now(), 1000.0);
        assert!(w.set(6.0, 1001.0));
    }

    #[test]
    fn moved_twice_at_one_engine_time_is_one_change() {
        let mut w = WallEpoch::new(0.0);
        assert!(w.set(3.0, 10.0));
        assert!(w.set(3.0, 20.0));
        // (And an engine time gone back, which it shouldn't, changes the latest rather than reordering.)
        assert!(w.set(2.0, 30.0));
        assert_eq!(w.len(), 2);
        assert_eq!((w.at(2.9), w.at(3.0)), (0.0, 30.0));
    }

    #[test]
    fn steps_both_ways_apply_at_once() {
        let mut w = WallEpoch::new(0.0);
        // A clock stepped 30 s forward (or a sleep), then 2 s back.
        assert!(w.set(100.0, 30_000.0));
        assert!(w.set(200.0, 28_000.0));
        assert_eq!((w.ms(150.0, 150.0), w.ms(250.0, 250.0)), (180_000.0, 278_000.0));
    }

    #[test]
    fn old_epochs_go_but_the_one_in_effect_stays() {
        let mut w = WallEpoch::new(0.0);
        w.set(10.0, 5.0);
        // Long after: the epoch from 10 s is still the one in effect until now.
        w.set(KEEP_S + 100.0, 10.0);
        assert_eq!(w.len(), 2);
        assert_eq!(w.at(50.0), 5.0, "a call up since 50 s keeps its epoch");
        assert_eq!(w.at(0.0), 5.0, "older than anything kept: the oldest kept");
        w.set(2.0 * KEEP_S + 200.0, 15.0);
        assert_eq!(w.len(), 2);
        assert_eq!(w.at(KEEP_S + 150.0), 10.0);
    }

    #[test]
    fn a_fast_drifting_clock_keeps_a_bounded_history() {
        // 500 ppm (far past any crystal), set every 100 ms for 12 hours: a move of 1 ms each 2 s.
        let mut w = WallEpoch::new(0.0);
        let mut t = 0.0;
        while t < 12.0 * 3600.0 {
            w.set(t, t * 0.5);
            t += 0.1;
        }
        assert!(w.len() <= KEEP_MAX && w.len() <= (KEEP_S / 2.0) as usize + 2, "{}", w.len());
        assert!((w.now() - t * 0.5).abs() < MIN_MOVE_MS + 0.1);
    }
}
