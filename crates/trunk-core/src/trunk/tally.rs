//! What became of the calls a trunked site or a conventional channel heard, counted where they
//! happen and at the finest grain: each copy of a call (a call heard on two sites is two
//! copies) once, when it ends, by what finally became of it, with the vocoder frames it decoded
//! (every copy's, saved or not). They go out as series under the site's or channel's key
//! ([`CopyTally::report`]): a system's figures are the sum of its sites' and channels'.

use super::calls::{Call, Reason};
use super::frames::{Codec, FrameErrors};
use crate::metrics::{Scoped, Sink};

/// Vocoder frames decoded.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameCount {
    pub frames: u64,
    /// Repeated, muted or lost: too damaged to play.
    pub bad: u64,
    /// Bit errors FEC corrected.
    pub errors: u64,
}

impl FrameCount {
    pub fn of(e: &FrameErrors) -> FrameCount {
        e.intervals.iter().fold(FrameCount::default(), |n, i| FrameCount { frames: n.frames + i.frames as u64, bad: n.bad + i.bad_frames as u64, errors: n.errors + i.errors })
    }
}

/// A site's or conventional channel's copies of calls, since recording started.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CopyTally {
    /// Copies of calls: announced by a grant (trunked) or taken by a row (conventional).
    pub started: u64,
    /// Of those, recorded.
    pub recorded: u64,
    /// Of those, not recorded, by why ([`Reason::ALL`]'s order).
    not_recorded: [u64; Reason::ALL.len()],
    /// Conventional: transmissions no row took (another code, or none where every row has one).
    pub skipped: u64,
    /// By codec ([`Codec::ALL`]'s order).
    voice: [FrameCount; 2],
}

fn slot(r: Reason) -> usize {
    Reason::ALL.iter().position(|&x| x == r).unwrap_or(0)
}

fn codec_slot(c: Codec) -> usize {
    Codec::ALL.iter().position(|&x| x == c).unwrap_or(0)
}

impl CopyTally {
    /// Copy `c` ended, as it finally was (a call refused as an unknown talkgroup and taken when
    /// patched to a known one counts as recorded), having decoded `frames`.
    pub fn copy_end(&mut self, c: &Call, frames: FrameCount) {
        self.started += 1;
        if c.recording {
            self.recorded += 1;
        } else if let Some(r) = c.reason {
            self.not_recorded[slot(r)] += 1;
        }
        if let Some(codec) = c.voice.codec() {
            let v = &mut self.voice[codec_slot(codec)];
            *v = FrameCount { frames: v.frames + frames.frames, bad: v.bad + frames.bad, errors: v.errors + frames.errors };
        }
    }

    /// A call that never became one, for reason `r` (a grant on a channel never resolved).
    pub fn missed(&mut self, r: Reason) {
        self.started += 1;
        self.not_recorded[slot(r)] += 1;
    }

    pub fn not_recorded(&self, r: Reason) -> u64 {
        self.not_recorded[slot(r)]
    }

    pub fn voice(&self, c: Codec) -> FrameCount {
        self.voice[codec_slot(c)]
    }

    /// As series: `calls/{started,recorded,skipped}`, `calls/why/<reason>` ([`Reason::key`]),
    /// `voice/<imbe|ambe>/{frames,bad,errors}`. A count is reported once it has counted
    /// something (a series that appears counts from 0).
    pub fn report(&self, sink: &mut dyn Sink) {
        let mut calls = Scoped::new(sink, "calls");
        calls.counter("started", self.started);
        calls.counter("recorded", self.recorded);
        if self.skipped > 0 {
            calls.counter("skipped", self.skipped);
        }
        let mut why = Scoped::new(&mut calls, "why");
        for r in Reason::ALL {
            if self.not_recorded(r) > 0 {
                why.counter(r.key(), self.not_recorded(r));
            }
        }
        for codec in Codec::ALL {
            let n = self.voice(codec);
            if n.frames > 0 {
                let mut v = Scoped::new(sink, format!("voice/{}", codec.key()));
                v.counter("frames", n.frames);
                v.counter("bad", n.bad);
                v.counter("errors", n.errors);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trunk::calls::Voice;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Counters(BTreeMap<String, u64>);
    impl Sink for Counters {
        fn counter(&mut self, name: &str, total: u64) {
            self.0.insert(name.to_string(), total);
        }
        fn gauge(&mut self, _: &str, _: f64) {}
    }

    #[test]
    fn each_copy_counts_once_by_what_finally_became_of_it_with_its_frames() {
        let mut t = CopyTally::default();
        let frames = FrameCount { frames: 50, bad: 2, errors: 30 };
        // Refused as unknown, then patched to a known talkgroup and recorded.
        t.copy_end(&Call { recording: true, reason: None, voice: Voice::P25, ..Default::default() }, frames);
        t.copy_end(&Call { reason: Some(Reason::NoRecorder), voice: Voice::P25Tdma { slot: 0 }, ..Default::default() }, FrameCount::default());
        t.copy_end(&Call { recording: true, voice: Voice::P25Tdma { slot: 1 }, ..Default::default() }, frames);
        t.missed(Reason::NoFrequency);
        let mut c = Counters::default();
        t.report(&mut c);
        let get = |k: &str| c.0.get(k).copied();
        assert_eq!((get("calls/started"), get("calls/recorded")), (Some(4), Some(2)));
        assert_eq!((get("calls/why/noRecorder"), get("calls/why/noFrequency"), get("calls/why/unknownTg")), (Some(1), Some(1), None));
        assert_eq!((get("voice/imbe/frames"), get("voice/ambe/frames"), get("voice/ambe/errors")), (Some(50), Some(50), Some(30)));
        assert_eq!(get("calls/skipped"), None, "trunked: none");
    }
}
