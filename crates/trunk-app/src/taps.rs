//! A signal tap's stream as files ([`crate::config::TapRoute`]): what its chunks
//! ([`TapChunk`]) come to once it has ended.
//!
//! Samples are a `.wav` (mono, 16-bit, at the tap's rate) with a `.jsonl` sidecar beside it;
//! frames and notes alone are the `.jsonl`. The sidecar's first line is the stream: its tap,
//! system, frequency, slot, call and talkgroup as it began, the time, what its data is, and
//! how many `items` (samples, frames or notes) it came to. Each line after it is one item
//! that something is said of, `at` its place in the stream: a frame's `bits` (hex) with its
//! note's fields beside them, or a note about the sample at `at`.

use serde_json::{json, Map, Value};
use trunk_recorder_plugin::{bits_hex, TapChunk, TapData, TapNote};

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

    /// Its next chunk.
    pub fn push(&mut self, chunk: TapChunk) {
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
            _ => vec![("jsonl", jsonl)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trunk_recorder_plugin::{AirFrame, Codec, Tap};

    fn chunk(tap: Tap, seq: u32, offset: u64, data: TapData, notes: Vec<TapNote>) -> TapChunk {
        TapChunk { tap, stream: 7, seq, first: seq == 0, last: false, offset, system: 0, system_id: 3, short_name: "dcfd".into(), freq_hz: 854_612_500.0, slot: 0, call_id: Some(9), talkgroup: Some(101), time: 1_800_000_000.0, data, notes }
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
}
