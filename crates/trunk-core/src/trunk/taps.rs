//! Signal taps: what a voice channel's receive chain holds part-way through, handed out as it is
//! made ([`trunk_recorder_plugin::tap`] has the taps and what they carry).
//!
//! A decoder asked for a tap ([`super::voice::VoiceSpec::taps`]) puts its pieces among its other
//! outputs ([`TrackerOut::Tap`]). The channel it decodes for, trunked or conventional, owns a
//! [`Streams`], which takes them out again ([`Streams::sift`]) and makes of each tap's pieces a
//! stream for each transmission: chunks in order, numbered, the first and last marked. That is
//! all a tap has to do to be saved and sent on; a tap that is off costs its decoder a branch.

use std::sync::atomic::{AtomicU64, Ordering};

pub use trunk_recorder_plugin::{AirFrame, FrameErrors, Tap, TapChunk, TapData, TapInfo, TapNote, TapSet, TAPS};

use super::record::TRANSMISSION_GAP_S;
use super::voice::TrackerOut;

/// A stream goes on this long at most, s: a carrier that never drops (a paging transmitter, a
/// stuck microphone) is cut into streams of this, so none is held or saved without end.
pub const MAX_STREAM_S: f64 = 600.0;

/// The FM discriminator tap's sample rate, Hz: what multimon-ng and its kind take.
pub const DISCRIMINATOR_RATE: u32 = 22_050;

/// What a decoder made for a tap: some data and the notes about it (`at` counted in this piece).
#[derive(Clone, Debug, PartialEq)]
pub struct TapPiece {
    pub tap: Tap,
    pub data: TapData,
    pub notes: Vec<TapNote>,
}

impl TapPiece {
    /// One frame's bits and what's said of it.
    pub fn frame(tap: Tap, codec: super::frames::Codec, bits: &[u8], note: &impl serde::Serialize) -> TapPiece {
        TapPiece { tap, data: TapData::Frames { codec, frames: vec![bits.to_vec()] }, notes: vec![TapNote::of(0, note)] }
    }

    /// A note with no data.
    pub fn note(tap: Tap, note: &impl serde::Serialize) -> TapPiece {
        TapPiece { tap, data: TapData::None, notes: vec![TapNote::of(0, note)] }
    }
}

/// What a channel knows of one of its slots as a chunk is handed out: the call on it and its
/// talkgroup, the radio talking, and what its voice is, as far as each is known.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OnAir {
    pub call_id: Option<u32>,
    pub talkgroup: Option<u32>,
    pub unit: Option<u32>,
    /// [`TapChunk::voice_mode`].
    pub voice_mode: &'static str,
}

/// Each stream of a run has a number of its own.
static NEXT_STREAM: AtomicU64 = AtomicU64::new(1);

/// One tap's stream on one slot: a transmission, as far as it has come.
struct Stream {
    tap: Tap,
    slot: u8,
    id: u64,
    seq: u32,
    /// Items (samples, frames, notes of a tap without data) sent so far.
    offset: u64,
    /// Channel samples since its last piece, and since it began.
    idle: u64,
    age: u64,
    /// What the next chunk takes: its data, its notes, and whether any piece came for it.
    data: TapData,
    notes: Vec<TapNote>,
    fresh: bool,
}

impl Stream {
    fn items(&self) -> usize {
        if self.data == TapData::None { self.notes.len() } else { self.data.len() }
    }
}

/// A channel's taps: its decoder's pieces made into streams.
pub struct Streams {
    set: TapSet,
    system: u16,
    freq_hz: f64,
    /// The gap that ends a stream, and the longest one goes on, in channel samples.
    gap: u64,
    max: u64,
    open: Vec<Stream>,
    /// Each slot's transmission is encrypted on the air, as far as the decoder has said: what
    /// is vocoded of it was decrypted ([`TapChunk::decrypted`]).
    secret: [bool; 2],
    /// Chunks cut and not yet handed out.
    ready: Vec<TapChunk>,
}

impl Streams {
    /// For a channel of system `system` (its number in the run) at `freq_hz`, decoded at `rate`
    /// samples a second, with `set` turned on.
    pub fn new(set: TapSet, system: u16, freq_hz: f64, rate: f64) -> Streams {
        Streams { set, system, freq_hz, gap: (TRANSMISSION_GAP_S * rate) as u64, max: (MAX_STREAM_S * rate) as u64, open: Vec::new(), secret: [false; 2], ready: Vec::new() }
    }

    /// The taps its decoder is asked for.
    pub fn set(&self) -> TapSet {
        self.set
    }

    /// One of the decoder's outputs for `slot`: a tap's piece is kept (None), anything else is
    /// handed back, noted on the way where a tap is made of it (a frame vocoded, a
    /// transmission's encryption and its end).
    pub fn sift(&mut self, slot: usize, o: TrackerOut) -> Option<TrackerOut> {
        if self.set.is_empty() {
            return Some(o);
        }
        let slot = slot as u8 & 1;
        match o {
            TrackerOut::Tap(p) => {
                self.piece(slot, p);
                None
            }
            TrackerOut::Audio(_, ref f) => {
                let note = FrameErrors::of(f);
                if self.set.has(Tap::VoiceFrames) {
                    self.piece(slot, TapPiece::frame(Tap::VoiceFrames, f.codec, &f.bits, &note));
                }
                if self.set.has(Tap::VoiceErrors) {
                    self.piece(slot, TapPiece::note(Tap::VoiceErrors, &note));
                }
                Some(o)
            }
            // (Told before the voice it's of is vocoded: a frame nothing has said of yet is held for it.)
            TrackerOut::Info { flags, .. } if flags.encrypted => {
                self.secret[slot as usize] = true;
                Some(o)
            }
            TrackerOut::Decrypted => {
                self.secret[slot as usize] = true;
                Some(o)
            }
            TrackerOut::Ended => {
                self.end(|s| s.slot == slot);
                self.secret[slot as usize] = false;
                Some(o)
            }
            o => Some(o),
        }
    }

    fn piece(&mut self, slot: u8, p: TapPiece) {
        if !self.set.has(p.tap) {
            return;
        }
        let i = match self.open.iter().position(|s| s.tap == p.tap && s.slot == slot) {
            Some(i) => i,
            None => {
                let id = NEXT_STREAM.fetch_add(1, Ordering::Relaxed);
                self.open.push(Stream { tap: p.tap, slot, id, seq: 0, offset: 0, idle: 0, age: 0, data: p.data.format(), notes: Vec::new(), fresh: false });
                self.open.len() - 1
            }
        };
        // (Data of another kind — a channel gone from one vocoder to another — starts a chunk of its own.)
        if self.open[i].data.format() != p.data.format() {
            self.cut(i, false);
            self.open[i].data = p.data.format();
        }
        let s = &mut self.open[i];
        let before = s.items() as u32;
        s.data.append(p.data);
        s.notes.extend(p.notes.into_iter().map(|n| TapNote { at: n.at + before, ..n }));
        s.fresh = true;
        s.idle = 0;
    }

    /// Stream `i`'s next chunk, of what it holds (`last`: nothing, when it holds nothing).
    fn cut(&mut self, i: usize, last: bool) {
        let s = &mut self.open[i];
        if !s.fresh && !last {
            return;
        }
        let items = s.items() as u64;
        let data = std::mem::replace(&mut s.data, TapData::None);
        s.data = data.format();
        self.ready.push(TapChunk {
            tap: s.tap,
            stream: s.id,
            seq: s.seq,
            first: s.seq == 0,
            last,
            offset: s.offset,
            system: self.system,
            system_id: 0,
            short_name: String::new(),
            freq_hz: self.freq_hz,
            slot: s.slot,
            call_id: None,
            talkgroup: None,
            unit: None,
            voice_mode: String::new(),
            decrypted: s.tap == Tap::VoiceFrames && self.secret[s.slot as usize & 1],
            time: 0.0,
            data,
            notes: std::mem::take(&mut s.notes),
        });
        s.seq += 1;
        s.offset += items;
        s.fresh = false;
    }

    /// End the streams `which` picks: each one's last chunk. (A transmission whose vocoded
    /// frames end is over: the next is encrypted only if it says so.)
    fn end(&mut self, which: impl Fn(&Stream) -> bool) {
        let mut i = 0;
        while i < self.open.len() {
            if which(&self.open[i]) {
                self.cut(i, true);
                let s = self.open.remove(i);
                if s.tap == Tap::VoiceFrames {
                    self.secret[s.slot as usize & 1] = false;
                }
            } else {
                i += 1;
            }
        }
    }

    /// `n` more channel samples were decoded and sifted: each stream's chunk of them, and the
    /// end of those the air has left for the gap between transmissions (or that have gone on
    /// too long). `who`: what the channel knows of a slot now. The
    /// chunks go to `out`, short of what only the host knows (the system's name, the time).
    pub fn flush(&mut self, n: usize, who: impl Fn(u8) -> OnAir, out: &mut Vec<TapChunk>) {
        if self.open.is_empty() && self.ready.is_empty() {
            return;
        }
        for i in 0..self.open.len() {
            let fresh = self.open[i].fresh;
            self.cut(i, false);
            let s = &mut self.open[i];
            s.age += n as u64;
            if !fresh {
                s.idle += n as u64;
            }
        }
        let (gap, max) = (self.gap, self.max);
        self.end(|s| s.idle >= gap || s.age >= max);
        self.hand(who, out);
    }

    /// The channel closed: every stream ends.
    pub fn close(&mut self, who: impl Fn(u8) -> OnAir, out: &mut Vec<TapChunk>) {
        self.end(|_| true);
        self.hand(who, out);
    }

    fn hand(&mut self, who: impl Fn(u8) -> OnAir, out: &mut Vec<TapChunk>) {
        out.extend(self.ready.drain(..).map(|mut c| {
            let on = who(c.slot);
            (c.call_id, c.talkgroup, c.unit, c.voice_mode) = (on.call_id, on.talkgroup, on.unit, on.voice_mode.to_string());
            c
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mbe::Kind;
    use crate::trunk::frames::{Codec, VoiceFrame};

    const RATE: f64 = 24_000.0;

    fn pcm(n: usize) -> TrackerOut {
        TrackerOut::Tap(TapPiece { tap: Tap::FmDiscriminator, data: TapData::Pcm { rate: DISCRIMINATOR_RATE, samples: vec![1; n] }, notes: Vec::new() })
    }

    fn voice(errs: u32) -> TrackerOut {
        TrackerOut::Audio(vec![0.0; 160], VoiceFrame { codec: Codec::Imbe, bits: vec![0; 88], e0: 0, errs, erased: false, kind: Kind::Voice, tone: None })
    }

    fn nobody(_: u8) -> OnAir {
        OnAir::default()
    }

    /// With no tap on, everything is handed back and nothing is made.
    #[test]
    fn off_costs_nothing() {
        let mut s = Streams::new(TapSet::NONE, 0, 1.0, RATE);
        assert!(matches!(s.sift(0, voice(1)), Some(TrackerOut::Audio(..))));
        // (A piece nobody asked for is handed back too: the channel drops it as it would anything it doesn't take.)
        assert!(s.sift(0, pcm(10)).is_some());
        let mut out = Vec::new();
        s.flush(2400, nobody, &mut out);
        s.close(nobody, &mut out);
        assert!(out.is_empty());
    }

    /// A stream is a transmission: its chunks numbered, each block's pieces one chunk, ended by
    /// the gap between transmissions; what comes after is another stream.
    #[test]
    fn a_stream_is_a_transmission() {
        let mut s = Streams::new(TapSet::of([Tap::FmDiscriminator]), 2, 152.0e6, RATE);
        let mut out = Vec::new();
        assert!(s.sift(0, pcm(100)).is_none());
        assert!(s.sift(0, pcm(50)).is_none());
        s.flush(2400, |_| OnAir { call_id: Some(7), talkgroup: Some(101), unit: Some(9), voice_mode: "fm" }, &mut out);
        s.sift(0, pcm(10));
        s.flush(2400, nobody, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].seq, out[0].first, out[0].last, out[0].offset, out[0].data.len()), (0, true, false, 0, 150));
        assert_eq!((out[1].seq, out[1].first, out[1].last, out[1].offset, out[1].data.len()), (1, false, false, 150, 10));
        assert_eq!((out[0].system, out[0].freq_hz, out[0].call_id, out[0].talkgroup, out[0].unit, out[0].voice_mode.as_str()), (2, 152.0e6, Some(7), Some(101), Some(9), "fm"));
        assert_eq!(out[0].stream, out[1].stream);
        // Quiet for the gap: over, with a last chunk that carries nothing.
        out.clear();
        for _ in 0..5 {
            s.flush(2400, nobody, &mut out);
        }
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].last && out[0].data.is_empty() && out[0].offset == 160);
        let first = out[0].stream;
        // The next transmission: a new stream.
        out.clear();
        s.sift(0, pcm(10));
        s.flush(2400, nobody, &mut out);
        assert!(out[0].first && out[0].stream != first && out[0].offset == 0);
        // The channel closing ends it.
        out.clear();
        s.close(nobody, &mut out);
        assert!(out.len() == 1 && out[0].last);
        s.close(nobody, &mut out);
        assert_eq!(out.len(), 1, "once");
    }

    /// A tap of notes alone: each frame vocoded is a note, counted through the stream; the
    /// transmission's terminator ends the stream at once, on its own slot only.
    #[test]
    fn frame_errors_are_notes_and_a_terminator_ends_them() {
        let mut s = Streams::new(TapSet::of([Tap::VoiceErrors]), 0, 1.0, RATE);
        let mut out = Vec::new();
        for errs in [0, 3] {
            assert!(s.sift(0, voice(errs)).is_some(), "the voice goes on to its call");
        }
        s.sift(1, voice(9));
        s.flush(2400, nobody, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].slot, out[0].data.clone(), out[0].items()), (0, TapData::None, 2));
        let notes = out[0].notes_as::<FrameErrors>();
        assert_eq!(notes.iter().map(|(at, n)| (*at, n.errs)).collect::<Vec<_>>(), [(0, 0), (1, 3)]);
        out.clear();
        s.sift(0, voice(1));
        assert!(matches!(s.sift(0, TrackerOut::Ended), Some(TrackerOut::Ended)));
        s.flush(2400, nobody, &mut out);
        assert_eq!(out.len(), 1);
        assert!(out[0].last && out[0].slot == 0 && out[0].offset == 2 && out[0].notes[0].at == 0);
        out.clear();
        s.close(nobody, &mut out);
        assert!(out.len() == 1 && out[0].slot == 1 && out[0].last);
    }

    /// The frames a vocoder was given are a tap: each with its note is the frame again. Those of a
    /// transmission the air says is encrypted are marked decrypted, and the next transmission's aren't.
    #[test]
    fn vocoded_frames_are_a_tap_and_say_when_they_were_decrypted() {
        use crate::trunk::calls::Flags;
        let mut s = Streams::new(TapSet::of([Tap::VoiceFrames, Tap::VoiceErrors]), 0, 1.0, RATE);
        let mut out = Vec::new();
        let frame = |errs| match voice(errs) {
            TrackerOut::Audio(_, f) => f,
            _ => unreachable!(),
        };
        s.sift(0, voice(2));
        s.sift(0, voice(5));
        s.flush(2400, nobody, &mut out);
        let of = |out: &[TapChunk], tap| out.iter().filter(|c| c.tap == tap).cloned().collect::<Vec<_>>();
        let frames = of(&out, Tap::VoiceFrames);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].voice_frames(), [(0, frame(2)), (1, frame(5))]);
        assert!(!frames[0].decrypted);
        assert_eq!(of(&out, Tap::VoiceErrors)[0].notes.len(), 2, "its errors, as before");
        s.sift(0, TrackerOut::Ended);
        s.flush(0, nobody, &mut out);
        // An encrypted transmission, on this slot only.
        out.clear();
        s.sift(0, TrackerOut::Info { source: None, flags: Flags { encrypted: true, ..Default::default() } });
        s.sift(0, voice(0));
        s.sift(1, voice(0));
        s.flush(2400, nobody, &mut out);
        let frames = of(&out, Tap::VoiceFrames);
        assert_eq!(frames.iter().map(|c| (c.slot, c.decrypted)).collect::<Vec<_>>(), [(0, true), (1, false)]);
        assert!(of(&out, Tap::VoiceErrors).iter().all(|c| !c.decrypted), "its error counts are no secret");
        // Its terminator: the next one is clear again. (So is one after the gap, with no terminator heard.)
        s.sift(0, TrackerOut::Ended);
        s.flush(0, nobody, &mut out);
        assert!(out.iter().any(|c| c.tap == Tap::VoiceFrames && c.last && c.decrypted), "its last chunk is its own");
        out.clear();
        s.sift(0, voice(0));
        s.flush(2400, nobody, &mut out);
        assert!(!of(&out, Tap::VoiceFrames)[0].decrypted);
        s.sift(0, TrackerOut::Decrypted);
        for _ in 0..6 {
            s.flush(2400, nobody, &mut out);
        }
        out.clear();
        s.sift(0, voice(0));
        s.flush(2400, nobody, &mut out);
        assert!(of(&out, Tap::VoiceFrames).iter().all(|c| c.first && !c.decrypted));
    }

    /// A carrier that never drops is cut into streams of [`MAX_STREAM_S`].
    #[test]
    fn a_stream_has_a_longest() {
        let mut s = Streams::new(TapSet::of([Tap::FmDiscriminator]), 0, 1.0, RATE);
        let mut out = Vec::new();
        let block = RATE as usize;
        for _ in 0..(MAX_STREAM_S as usize + 2) {
            s.sift(0, pcm(8));
            s.flush(block, nobody, &mut out);
        }
        let streams: std::collections::BTreeSet<u64> = out.iter().map(|c| c.stream).collect();
        assert_eq!(streams.len(), 2);
        assert_eq!(out.iter().filter(|c| c.last).count(), 1);
    }
}
