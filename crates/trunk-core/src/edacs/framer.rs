//! The EDACS outbound control channel's framing: every 30 ms (288 bits at
//! 9600 baud) a 48-bit sync, then two messages, each sent three times — the
//! middle copy inverted — as a 40-bit BCH word (28 bits of data).
//!
//! ```text
//! 0x555557125555 │ msg 1 │ ¬msg 1 │ msg 1 │ msg 2 │ ¬msg 2 │ msg 2
//!     48 bits      40       40       40      40      40       40
//! ```
//!
//! The three copies are voted bit by bit (on their soft values) and the vote
//! is BCH-decoded; it's kept when two of the copies agree with it.

use super::bch;

/// The frame sync, first bit sent highest.
pub const SYNC: u64 = 0x5555_5712_5555;
pub const SYNC_BITS: usize = 48;
/// Bits after the sync: two messages, three copies of 40 bits each.
pub const BODY_BITS: usize = 240;
/// Bits a copy of a message may be off from the corrected word.
const COPY_ERRORS: u32 = 4;

/// One frame's two messages (28 bits each; None: lost), and when its first
/// body bit was heard.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub msgs: [Option<u32>; 2],
    pub t: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    Hunt,
    /// Reading the body; the signal's polarity.
    Body { n: usize, inverted: bool },
    /// A sync is due after the frame just read.
    Due { n: usize, inverted: bool },
}

/// Hunting for a sync, reading the body after it, and checking the next
/// sync where it's due: what the control channel's framing and ProVoice's share.
pub struct Sync {
    words: &'static [u64],
    bits: usize,
    /// Sync bits that may be wrong when hunting / when one is due.
    hunt: u32,
    due: u32,
    sr: u64,
    state: State,
    /// The last body read, upright, and when its first bit was heard.
    pub body: Vec<f32>,
    pub t: f64,
    /// Frames in a row whose sync came where it was due.
    pub run: u32,
}

impl Sync {
    /// `words`: the syncs (`bits` long, first bit sent highest) a frame may start with.
    pub fn new(words: &'static [u64], bits: usize, body_bits: usize, hunt: u32, due: u32) -> Self {
        Sync { words, bits, hunt, due, sr: 0, state: State::Hunt, body: vec![0.0; body_bits], t: 0.0, run: 0 }
    }

    pub fn in_sync(&self) -> bool {
        self.run > 0 && self.state != State::Hunt
    }

    /// One bit (`soft` > 0: a 1 as sent upright), heard at `t`; true when it
    /// ends a body ([`Sync::body`]).
    pub fn push(&mut self, soft: f32, t: f64) -> bool {
        self.sr = (self.sr << 1 | (soft > 0.0) as u64) & ((1 << self.bits) - 1);
        match self.state {
            State::Hunt => {
                if let Some(inverted) = self.sync(self.hunt) {
                    self.state = State::Body { n: 0, inverted };
                }
                false
            }
            State::Body { n, inverted } => {
                if n == 0 {
                    self.t = t;
                }
                self.body[n] = if inverted { -soft } else { soft };
                if n + 1 < self.body.len() {
                    self.state = State::Body { n: n + 1, inverted };
                    return false;
                }
                self.state = State::Due { n: 0, inverted };
                true
            }
            State::Due { n, inverted } => {
                if n + 1 < self.bits {
                    self.state = State::Due { n: n + 1, inverted };
                    return false;
                }
                match self.sync(self.due) {
                    Some(inv) if inv == inverted => {
                        self.run += 1;
                        self.state = State::Body { n: 0, inverted };
                    }
                    _ => {
                        self.run = 0;
                        self.state = State::Hunt;
                    }
                }
                false
            }
        }
    }

    /// The last bits are a sync with at most `max` errors: upright (false)
    /// or inverted (true).
    fn sync(&self, max: u32) -> Option<bool> {
        self.words.iter().find_map(|&w| {
            let e = (self.sr ^ w).count_ones();
            if e <= max {
                Some(false)
            } else if self.bits as u32 - e <= max {
                Some(true)
            } else {
                None
            }
        })
    }
}

pub struct Framer {
    pub sync: Sync,
    /// Messages decoded / lost.
    pub good: u64,
    pub bad: u64,
}

impl Default for Framer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framer {
    pub fn new() -> Self {
        // Sync bits that may be wrong when hunting for a frame / when one is due.
        Framer { sync: Sync::new(&[SYNC], SYNC_BITS, BODY_BITS, 4, 12), good: 0, bad: 0 }
    }

    /// One bit (`soft` > 0: a 1 as sent upright), heard at `t`.
    pub fn push(&mut self, soft: f32, t: f64) -> Option<Frame> {
        if !self.sync.push(soft, t) {
            return None;
        }
        let msgs = decode(&self.sync.body);
        for m in msgs {
            if m.is_some() {
                self.good += 1;
            } else {
                self.bad += 1;
            }
        }
        Some(Frame { msgs, t: self.sync.t })
    }
}

/// A body's two messages.
fn decode(body: &[f32]) -> [Option<u32>; 2] {
    std::array::from_fn(|k| {
        let at = k * 120;
        let copy = |c: usize, invert: bool| -> u64 {
            (0..40).fold(0u64, |w, i| {
                let v = body[at + 40 * c + i];
                w << 1 | ((v > 0.0) != invert) as u64
            })
        };
        let vote = (0..40).fold(0u64, |w, i| {
            let v = body[at + i] - body[at + 40 + i] + body[at + 80 + i];
            w << 1 | (v > 0.0) as u64
        });
        // The vote, corrected, stands only when two of the copies are
        // close to it too: a word of noise passes the BCH check one time
        // in five (two corrections reach that much of the space).
        let copies = [copy(0, false), copy(1, true), copy(2, false)];
        bch::decode(vote).filter(|&(d, _)| {
            let cw = bch::encode(d);
            copies.iter().filter(|&&c| (c ^ cw).count_ones() <= COPY_ERRORS).count() >= 2
        }).map(|(d, _)| d)
    })
}

/// The bits of a frame carrying `m1` and `m2` (for test signals and the bench).
pub fn encode(m1: u32, m2: u32) -> Vec<u8> {
    let mut bits = Vec::with_capacity(SYNC_BITS + BODY_BITS);
    bits.extend((0..SYNC_BITS).rev().map(|i| (SYNC >> i & 1) as u8));
    for m in [m1, m2] {
        let w = bch::encode(m);
        for inv in [0u8, 1, 0] {
            bits.extend((0..40).rev().map(|i| (w >> i & 1) as u8 ^ inv));
        }
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_after_a_sync_decodes_nothing() {
        let mut f = Framer::new();
        let mut s = 0x9e37_79b9u32;
        let mut got = 0;
        for _ in 0..2000 {
            // A sync, then 240 bits of noise.
            let mut bits: Vec<u8> = (0..SYNC_BITS).rev().map(|i| (SYNC >> i & 1) as u8).collect();
            bits.extend((0..BODY_BITS).map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s & 1) as u8
            }));
            for b in bits {
                if let Some(fr) = f.push(if b == 1 { 1.0 } else { -1.0 }, 0.0) {
                    got += fr.msgs.iter().filter(|m| m.is_some()).count();
                }
            }
        }
        assert_eq!(got, 0, "messages out of 4000 words of noise");
    }

    fn run(bits: &[u8], flip: bool) -> (Framer, Vec<Frame>) {
        let mut f = Framer::new();
        let out = bits.iter().enumerate().filter_map(|(i, &b)| f.push(if (b == 1) != flip { 1.0 } else { -1.0 }, i as f64)).collect();
        (f, out)
    }

    #[test]
    fn frames_both_polarities_and_votes_out_errors() {
        let mut bits = vec![1, 0, 0, 1, 1];
        for k in 0..10u32 {
            bits.extend(encode(0x0123456 + k, 0x0FEDCBA - k));
        }
        // Three errors in one copy of the first message, and one in another copy: the vote fixes them.
        for i in [60, 61, 62, 100] {
            bits[i] ^= 1;
        }
        for flip in [false, true] {
            let (f, out) = run(&bits, flip);
            assert_eq!(out.len(), 10, "flip {flip}");
            for (k, fr) in out.iter().enumerate() {
                assert_eq!(fr.msgs, [Some(0x0123456 + k as u32), Some(0x0FEDCBA - k as u32)]);
            }
            assert_eq!((f.good, f.bad), (20, 0));
            assert!(f.sync.in_sync());
        }
    }
}
