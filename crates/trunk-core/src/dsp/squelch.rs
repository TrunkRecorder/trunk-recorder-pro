//! The squelch gate analog voice (FM and AM) shares: carrier power against
//! a threshold opens and closes it, and audio passes only while it's open.
//!
//! ```text
//! per channel sample:  slow meter (the demod's, ~10 ms) up for `open` → opens
//!                      fast meter (a boxcar) down for `hang` → closes
//!                      (or a carrier the demod detected itself: Gate::decide)
//! per 8 kHz sample:    into a delay line, marked with the gate; out of it,
//!                      only what the gate let through
//! ```
//!
//! The delay line is what keeps the edges clean. When the gate closes, the
//! audio since the carrier went (and a guard before it, for the meter's lag
//! and the audio filter's) is still in the line: it is cut there and then,
//! with a ramp down before it, so no squelch tail is heard however long the
//! hang. When it opens, the audio of the last few ms of carrier is let
//! through with a ramp up, so a transmission doesn't lose its start to the
//! open delay. A fade shorter than the hang keeps the gate open.
//!
//! The line is the hang plus the guard and a ramp long (15 ms at the
//! defaults; more behind a slower detector), which is how far behind the
//! air the audio runs.

use num_complex::Complex32;

use super::fm::AUDIO_RATE;

/// How the gate opens and closes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SquelchTiming {
    /// The carrier must hold this long before the gate opens, s (a
    /// neighbour keying up splatters for an instant).
    pub open_s: f64,
    /// The gate stays open this long after the carrier goes, s: a longer
    /// hang bridges fades on a weak signal (the audio runs this much later).
    pub hang_s: f64,
}

impl Default for SquelchTiming {
    fn default() -> Self {
        SquelchTiming { open_s: 0.030, hang_s: 0.002 }
    }
}

/// Gate ramp, s.
const RAMP_S: f64 = 0.010;
/// Audio let through from before the gate opened, at most, s.
const LEAD_S: f64 = 0.015;
/// The guard before the carrier's going, past the fast meter's own window, s
/// (the audio low-pass's delay, and some).
const GUARD_S: f64 = 0.004;

pub struct Gate {
    /// Channel samples per 8 kHz sample.
    step: f64,
    /// Channel samples the slow meter has been up for, and how many open the gate.
    up_run: u32,
    open_after: u32,
    /// Fast meter: |y|² over the last `fast.len()` channel samples, and their sum.
    fast: Vec<f32>,
    fast_pos: usize,
    fast_sum: f64,
    /// Channel samples the fast meter has been under the threshold (and over it).
    down_run: u32,
    up_streak: u32,
    hang: u32,
    /// Opening: the carrier's first output samples not let through.
    open_margin: usize,
    open: bool,
    /// The gate as the last 8 kHz sample saw it.
    was_open: bool,
    /// (audio, raw audio, gain) waiting to go out.
    line: Vec<(f32, f32, f32)>,
    pos: usize,
    gain: f32,
    ramp: f32,
    ramp_n: usize,
    lead: usize,
    guard: usize,
    /// Held shut by the demod (data on the channel, not voice): see [`Gate::mute`].
    muted: bool,
}

impl Gate {
    /// `rate`: the channel rate. `fast_s`: the fast meter's window — short
    /// for FM (1 ms), long enough for AM not to read its modulation's
    /// troughs as the carrier going.
    pub fn new(rate: f64, timing: SquelchTiming, fast_s: f64) -> Self {
        Self::build(rate, timing, fast_s, fast_s + GUARD_S, fast_s + GUARD_S, LEAD_S)
    }

    /// A gate whose carrier the demod decides ([`Gate::decide`]), with a
    /// detector that tells the carrier has gone up to `lag_s` late. Opening,
    /// the audio of all but the first ms of the carrier it has seen is let
    /// through, up to the open delay's worth.
    pub fn detected(rate: f64, timing: SquelchTiming, lag_s: f64) -> Self {
        Self::build(rate, timing, 0.0, lag_s + GUARD_S, GUARD_S, timing.open_s + RAMP_S)
    }

    fn build(rate: f64, timing: SquelchTiming, fast_s: f64, guard_s: f64, open_margin_s: f64, lead_s: f64) -> Self {
        let out = |s: f64| (s * AUDIO_RATE).round() as usize;
        let guard = out(guard_s);
        let ramp_n = out(RAMP_S);
        let hang = ((timing.hang_s * rate) as u32).max(1);
        let len = out(timing.hang_s).max(1) + guard + ramp_n + 2;
        Gate {
            step: rate / AUDIO_RATE,
            up_run: 0,
            open_after: (timing.open_s * rate) as u32,
            fast: vec![0.0; ((fast_s * rate).round() as usize).max(1)],
            fast_pos: 0,
            fast_sum: 0.0,
            down_run: 0,
            up_streak: 0,
            hang,
            open_margin: out(open_margin_s),
            open: false,
            was_open: false,
            line: vec![(0.0, 0.0, 0.0); len.max(out(lead_s) + 2)],
            pos: 0,
            gain: 0.0,
            ramp: 1.0 / ramp_n as f32,
            ramp_n,
            lead: out(lead_s),
            guard,
            muted: false,
        }
    }

    /// One channel sample: `y` filtered, `slow_up` whether the demod's slow
    /// meter reads above `open_power`. Returns whether the gate is open.
    #[inline]
    pub fn meter(&mut self, y: Complex32, slow_up: bool, open_power: f32) -> bool {
        let p = y.norm_sqr();
        self.fast_sum += (p - self.fast[self.fast_pos]) as f64;
        self.fast[self.fast_pos] = p;
        self.fast_pos = (self.fast_pos + 1) % self.fast.len();
        if self.fast_pos == 0 {
            // Re-sum now and then so rounding can't drift.
            self.fast_sum = self.fast.iter().map(|&v| v as f64).sum();
        }
        let fast_up = self.fast_sum > open_power as f64 * self.fast.len() as f64;
        self.decide(slow_up, fast_up)
    }

    /// One channel sample, the carrier decided by the caller: `slow_up` opens
    /// (held for the open delay), `fast_up` gone for the hang closes. Returns
    /// whether the gate is open.
    #[inline]
    pub fn decide(&mut self, slow_up: bool, fast_up: bool) -> bool {
        self.up_run = if slow_up { self.up_run.saturating_add(1) } else { 0 };
        if fast_up {
            self.down_run = 0;
            self.up_streak = self.up_streak.saturating_add(1);
        } else {
            self.down_run = self.down_run.saturating_add(1);
            self.up_streak = 0;
        }
        if self.open {
            self.open = self.down_run < self.hang;
        } else {
            // A fade too short to drop the slow meter reopens as soon as the carrier is back.
            self.open = fast_up && self.up_run >= self.open_after;
        }
        self.open
    }

    /// Shut the audio while `on` (the channel carries data, not voice),
    /// cutting the last `back` 8 kHz samples too (what the detector heard
    /// before it was sure). Cut audio is left out, as when the carrier's
    /// gone; it ramps back up after.
    pub fn mute(&mut self, on: bool, back: usize) {
        if on && !self.muted {
            let room = self.line.len() - 1;
            for k in 1..=back.min(room) {
                self.back(k).2 = -1.0;
            }
        }
        if on {
            self.gain = 0.0;
        }
        self.muted = on;
    }

    /// The line's `k`th newest entry (from 1).
    fn back(&mut self, k: usize) -> &mut (f32, f32, f32) {
        let n = self.line.len();
        &mut self.line[(self.pos + n - k) % n]
    }

    /// One 8 kHz sample (`raw`: the same before the demod's last filters),
    /// as the gate stands. What leaves the line, if the gate let it through,
    /// goes to `out` (and `raw` to `low`).
    #[inline]
    pub fn audio(&mut self, s: f32, raw: f32, out: &mut Vec<f32>, low: Option<&mut Vec<f32>>) {
        let room = self.line.len() - 1;
        if self.open && !self.was_open {
            // The carrier's last few ms (past the guard), ramped up.
            let up = (self.up_streak as f64 / self.step) as usize;
            let lead = self.lead.min(up.saturating_sub(self.open_margin)).min(room);
            self.gain = 0.0;
            for k in (1..=lead).rev() {
                self.gain = (self.gain + self.ramp).min(1.0);
                let g = self.gain;
                let e = self.back(k);
                // (Not what the demod cut: data, never voice.)
                if e.2 >= 0.0 {
                    e.2 = e.2.max(g);
                }
            }
        } else if !self.open && self.was_open {
            // Cut from the guard before the carrier went, ramping down before that.
            let cut = ((self.down_run as f64 / self.step) as usize + self.guard).min(room);
            for k in 1..=cut {
                self.back(k).2 = 0.0;
            }
            for i in 0..self.ramp_n.min(room - cut) {
                let f = (i + 1) as f32 / (self.ramp_n + 1) as f32;
                let e = self.back(cut + 1 + i);
                e.2 = e.2.min(f);
            }
            self.gain = 0.0;
        }
        self.was_open = self.open;
        // (Cut by the demod: −1, which an opening's lead doesn't let through either.)
        let g = if self.muted {
            -1.0
        } else if self.open {
            self.gain = (self.gain + self.ramp).min(1.0);
            self.gain
        } else {
            0.0
        };
        let (s, raw, g) = std::mem::replace(&mut self.line[self.pos], (s, raw, g));
        self.pos = (self.pos + 1) % self.line.len();
        if g > 0.0 {
            out.push(s * g);
            if let Some(l) = low {
                l.push(raw);
            }
        }
    }
}
