//! A signal tap's stream as files ([`crate::config::TapRoute`]): what its chunks
//! ([`TapChunk`]) come to once it has ended.
//!
//! Samples are a `.wav` (mono, 16-bit, at the tap's rate) with a `.jsonl` sidecar beside it;
//! frames and notes alone are the `.jsonl`. The sidecar's first line is the stream: its tap,
//! system, frequency, slot, the time it began, the call, talkgroup and talker (the first it
//! had of each), what its data is, and how many `items` (samples, frames or notes) it came
//! to. Each line after it is one item that something is said of, `at` its place in the
//! stream: a frame's `bits` (hex) with its note's fields beside them, or a note about the
//! sample at `at`.
//!
//! The vocoded frames (`voice.frames`) are also an `.sdr` file ([`trunk_core::trunk::sdr`]:
//! MimoSDR's container) where the format has a layout for the stream's voice: P25 and DMR.
//! Each of their sidecar's lines is a [`VoiceFrame`], so the sidecar alone is the stream for
//! every voice (NXDN and ProVoice have no other).

use serde_json::{json, Map, Value};
use trunk_core::trunk::sdr::{call_sdr, SdrInfo, SdrMode};
use trunk_recorder_plugin::{bits_hex, FrameErrors, Tap, TapChunk, TapData, TapNote, VoiceFrame};

/// A stream being gathered, from its first chunk to its last.
pub struct TapFile {
    /// Its first chunk, without its data: what the sidecar's first line is made of.
    head: TapChunk,
    data: TapData,
    /// Its notes, `at` counted from the stream's start.
    notes: Vec<(u64, Map<String, Value>)>,
    /// Items so far, and notes where there's no data.
    items: u64,
}

impl TapFile {
    /// A stream's first chunk.
    pub fn new(chunk: TapChunk) -> TapFile {
        let mut f = TapFile { head: TapChunk { data: chunk.data.format(), notes: Vec::new(), ..chunk.clone() }, data: chunk.data.format(), notes: Vec::new(), items: 0 };
        f.push(chunk);
        f
    }

    /// Its next chunk. (What the stream began without, it has from the first chunk with it:
    /// voice comes before its call.)
    pub fn push(&mut self, chunk: TapChunk) {
        let h = &mut self.head;
        (h.call_id, h.talkgroup, h.unit) = (h.call_id.or(chunk.call_id), h.talkgroup.or(chunk.talkgroup), h.unit.or(chunk.unit));
        if h.voice_mode.is_empty() {
            h.voice_mode.clone_from(&chunk.voice_mode);
        }
        h.decrypted |= chunk.decrypted;
        let n = chunk.items() as u64;
        self.notes.extend(chunk.notes.into_iter().map(|TapNote { at, fields }| (chunk.offset + at as u64, fields)));
        self.data.append(chunk.data);
        self.items = self.items.max(chunk.offset + n);
    }

    /// Its files, each as (extension, bytes).
    pub fn files(&self) -> Vec<(&'static str, Vec<u8>)> {
        let mut head = serde_json::to_value(&self.head).unwrap_or_default();
        if let Some(h) = head.as_object_mut() {
            // (A chunk's place in its stream isn't the stream's.)
            for k in ["seq", "first", "last", "offset"] {
                h.remove(k);
            }
            h.insert("items".into(), json!(self.items));
        }
        let mut lines = vec![head];
        let line = |at: u64, fields: &Map<String, Value>, bits: Option<&[u8]>| {
            let mut l = Map::new();
            l.insert("at".into(), json!(at));
            if let Some(b) = bits {
                l.insert("bits".into(), json!(bits_hex(b)));
            }
            l.extend(fields.clone());
            Value::Object(l)
        };
        let none = Map::new();
        match &self.data {
            TapData::Frames { frames, .. } => {
                let mut notes = self.notes.iter().peekable();
                for (i, f) in frames.iter().enumerate() {
                    while notes.next_if(|(at, _)| *at < i as u64).is_some() {}
                    let fields = notes.next_if(|(at, _)| *at == i as u64).map_or(&none, |(_, f)| f);
                    lines.push(line(i as u64, fields, Some(f)));
                }
            }
            _ => lines.extend(self.notes.iter().map(|(at, f)| line(*at, f, None))),
        }
        let jsonl = lines.iter().fold(String::new(), |s, l| s + &l.to_string() + "\n").into_bytes();
        match &self.data {
            TapData::Pcm { rate, samples } => vec![("wav", trunk_core::wav::encode_pcm16(samples, *rate)), ("jsonl", jsonl)],
            _ => self.sdr().map(|sdr| ("sdr", sdr)).into_iter().chain([("jsonl", jsonl)]).collect(),
        }
    }

    /// [`Tap::VoiceFrames`]: its frames, in order (one without its note is no frame: left out).
    pub fn voice_frames(&self) -> Vec<VoiceFrame> {
        let TapData::Frames { frames, .. } = &self.data else { return Vec::new() };
        let note = |(at, fields): &(u64, Map<String, Value>)| serde_json::from_value::<FrameErrors>(Value::Object(fields.clone())).ok()?.frame(frames.get(*at as usize)?);
        self.notes.iter().filter_map(note).collect()
    }

    /// The vocoded frames as an `.sdr` file, where the stream's voice has a layout there.
    fn sdr(&self) -> Option<Vec<u8>> {
        let h = &self.head;
        let mode = match (h.tap, h.voice_mode.as_str()) {
            (Tap::VoiceFrames, "p25") => SdrMode::P25Fdma,
            (Tap::VoiceFrames, "p25_phase2") => SdrMode::P25Tdma,
            (Tap::VoiceFrames, "dmr") => SdrMode::Dmr,
            _ => return None,
        };
        let frames = self.voice_frames();
        let unit = h.unit.unwrap_or(0);
        let info = SdrInfo {
            start_us: (h.time * 1e6).round().max(0.0) as u64,
            duration_ms: frames.len() as u32 * 20,
            hz: h.freq_hz.round() as u32,
            system: h.short_name.clone(),
            slot: h.slot,
            tg: h.talkgroup.unwrap_or(0),
            first_src: unit,
            src: unit,
            encrypted: h.decrypted,
            ..Default::default()
        };
        call_sdr(mode, &frames, &info)
    }
}

/// A saved stream's sidecar read back: its head, and the vocoded frames its lines are
/// ([`Tap::VoiceFrames`]'s; none of another tap's). None: not a sidecar.
pub fn sidecar_frames(text: &str) -> Option<(Value, Vec<VoiceFrame>)> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let head: Value = serde_json::from_str(lines.next()?).ok()?;
    head.get("tap")?;
    Some((head, lines.filter_map(|l| serde_json::from_str(l).ok()).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use trunk_recorder_plugin::{AirFrame, Codec, FrameKind};

    fn chunk(tap: Tap, seq: u32, offset: u64, data: TapData, notes: Vec<TapNote>) -> TapChunk {
        TapChunk { tap, stream: 7, seq, first: seq == 0, last: false, offset, system: 0, system_id: 3, short_name: "dcfd".into(), freq_hz: 854_612_500.0, slot: 0, call_id: Some(9), talkgroup: Some(101), unit: None, voice_mode: String::new(), decrypted: false, time: 1_800_000_000.0, data, notes }
    }

    fn lines(bytes: &[u8]) -> Vec<Value> {
        std::str::from_utf8(bytes).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    #[test]
    fn samples_are_a_wav_with_its_sidecar() {
        let pcm = |s: Vec<i16>| TapData::Pcm { rate: 22_050, samples: s };
        let mut f = TapFile::new(chunk(Tap::FmDiscriminator, 0, 0, pcm(vec![1, -2, 3]), Vec::new()));
        f.push(chunk(Tap::FmDiscriminator, 1, 3, pcm(vec![4, 5]), vec![TapNote::of(1, &json!({ "mark": true }))]));
        let files = f.files();
        assert_eq!(files.iter().map(|f| f.0).collect::<Vec<_>>(), ["wav", "jsonl"]);
        let (rate, samples) = trunk_core::wav::decode(&files[0].1).unwrap();
        assert_eq!((rate, samples.len()), (22_050, 5));
        assert_eq!(&files[0].1[44..], [1i16, -2, 3, 4, 5].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>(), "the samples as they came");
        let l = lines(&files[1].1);
        assert_eq!(l[0]["tap"], "fm.discriminator");
        assert_eq!((&l[0]["short_name"], &l[0]["talkgroup"], &l[0]["items"]), (&json!("dcfd"), &json!(101), &json!(5)));
        assert_eq!(l[0]["data"], json!({ "kind": "pcm", "rate": 22_050, "pcm": "" }));
        assert!(l[0].get("seq").is_none() && l[0].get("offset").is_none());
        assert_eq!(l[1], json!({ "at": 4, "mark": true }), "a note's place is its stream's");
    }

    #[test]
    fn frames_are_lines_each_with_its_note() {
        let frames = |n: usize| TapData::Frames { codec: Codec::Imbe, frames: vec![vec![1; 88]; n] };
        let note = |at: u32, n: u8| TapNote::of(at, &AirFrame { n: Some(n), errs: 2, ..Default::default() });
        let mut f = TapFile::new(chunk(Tap::VoiceAir, 0, 0, frames(2), vec![note(0, 0), note(1, 1)]));
        f.push(chunk(Tap::VoiceAir, 1, 2, frames(1), vec![note(0, 2)]));
        let files = f.files();
        assert_eq!(files.len(), 1);
        let l = lines(&files[0].1);
        assert_eq!(l.len(), 4);
        assert_eq!(l[0]["items"], 3);
        assert_eq!(l[3], json!({ "at": 2, "bits": "f".repeat(22), "n": 2, "errs": 2, "e0": 0 }));
    }

    /// The vocoded frames: an `.sdr` file where the voice has a layout, and a sidecar whose
    /// lines are the frames either way. What the stream began without (its call), it takes
    /// from the first chunk that has it.
    #[test]
    fn vocoded_frames_are_an_sdr_file_and_lines_that_are_frames() {
        let frame = |k: u32| VoiceFrame { codec: Codec::Imbe, bits: (0..88).map(|i| ((i + k) % 3 == 0) as u8).collect(), e0: 0, errs: k % 4, erased: false, kind: FrameKind::Voice, tone: None };
        let all: Vec<VoiceFrame> = (0..27).map(frame).collect();
        let part = |seq: u32, mode: &str, f: &[VoiceFrame], offset: u64| TapChunk {
            call_id: (seq > 0).then_some(12),
            talkgroup: (seq > 0).then_some(101),
            unit: (seq > 0).then_some(7001),
            voice_mode: mode.into(),
            decrypted: seq > 0,
            ..chunk(Tap::VoiceFrames, seq, offset, TapData::Frames { codec: f[0].codec, frames: f.iter().map(|f| f.bits.clone()).collect() }, f.iter().enumerate().map(|(i, f)| TapNote::of(i as u32, &FrameErrors::of(f))).collect())
        };
        let mut f = TapFile::new(part(0, "p25", &all[..9], 0));
        f.push(part(1, "p25", &all[9..], 9));
        assert_eq!(f.voice_frames(), all);
        let files = f.files();
        assert_eq!(files.iter().map(|f| f.0).collect::<Vec<_>>(), ["sdr", "jsonl"]);
        assert_eq!(trunk_core::trunk::sdr::sdr_frames(&files[0].1), all, "the .sdr holds the frames");
        let l = lines(&files[1].1);
        assert_eq!((&l[0]["call_id"], &l[0]["talkgroup"], &l[0]["unit"], &l[0]["voice_mode"], &l[0]["decrypted"], &l[0]["items"]), (&json!(12), &json!(101), &json!(7001), &json!("p25"), &json!(true), &json!(27)));
        let back: Vec<VoiceFrame> = l[1..].iter().map(|l| serde_json::from_value(l.clone()).unwrap()).collect();
        assert_eq!(back, all, "each line is a frame");
        let (head, read) = sidecar_frames(std::str::from_utf8(&files[1].1).unwrap()).unwrap();
        assert_eq!((&head["call_id"], read), (&json!(12), all.clone()), "and the sidecar reads back");
        assert!(sidecar_frames(&VoiceFrame::to_jsonl(&all)).is_none(), "bare frame lines aren't one");
        // A voice the format has no layout for: the sidecar alone. And the frames as they came off the air are no .sdr.
        let ambe: Vec<VoiceFrame> = (0..4).map(|k| VoiceFrame { codec: Codec::Ambe, bits: vec![(k % 2) as u8; 49], ..frame(k) }).collect();
        assert_eq!(TapFile::new(part(0, "nxdn48", &ambe, 0)).files().iter().map(|f| f.0).collect::<Vec<_>>(), ["jsonl"]);
        assert_eq!(TapFile::new(TapChunk { tap: Tap::VoiceAir, ..part(0, "p25", &all, 0) }).files().len(), 1);
    }
}
