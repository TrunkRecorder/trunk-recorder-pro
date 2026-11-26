//! Tags on a call's audio files (#196): what players show, and what scripts
//! read without the .json beside them. Trunk Recorder's three (title: the
//! talkgroup's alpha tag or number; artist: the system's short name; date),
//! then genre (the talkgroup's group), a comment for people, and fields for
//! scripts: TALKGROUP, UNITS (the radios that talked, in order; absent when
//! none was heard), FREQUENCY (Hz), EMERGENCY and ENCRYPTED (1, when so).
//!
//! The WAV carries them as an ID3v2.4 tag in an `id3 ` chunk after its audio;
//! the M4A as iTunes tags (`©nam` …, the script fields as `----` atoms of
//! `com.apple.iTunes`), written here, so every encoder's file has the same.

use trunk_recorder_plugin::CallRecord;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tags {
    pub title: String,
    pub artist: String,
    /// Local time, `2026-11-25T14:03:11`.
    pub date: String,
    pub genre: String,
    pub comment: String,
    /// For scripts: (name, value).
    pub fields: Vec<(&'static str, String)>,
}

impl Tags {
    /// A call's, from its record; `utc_offset_s`: local time's offset at its start.
    pub fn of(r: &CallRecord, utc_offset_s: i32) -> Tags {
        let mut units: Vec<&trunk_recorder_plugin::SrcEntry> = Vec::new();
        for s in r.src_list.iter().filter(|s| s.src > 0) {
            if !units.iter().any(|u| u.src == s.src) {
                units.push(s);
            }
        }
        let ids = units.iter().map(|u| u.src.to_string()).collect::<Vec<_>>().join(", ");
        let named = units.iter().map(|u| if u.tag.is_empty() { u.src.to_string() } else { format!("{} ({})", u.src, u.tag) }).collect::<Vec<_>>().join(", ");
        let mhz = format!("{:.6}", r.freq as f64 / 1e6).trim_end_matches('0').trim_end_matches('.').to_string();
        let mut comment = vec![format!("TG {}", r.talkgroup), format!("{mhz} MHz")];
        if !units.is_empty() {
            comment.push(format!("{} {named}", if units.len() == 1 { "Unit" } else { "Units" }));
        }
        let mut fields = vec![("TALKGROUP", r.talkgroup.to_string())];
        if !units.is_empty() {
            fields.push(("UNITS", ids));
        }
        fields.push(("FREQUENCY", r.freq.to_string()));
        for (on, name, said) in [(r.emergency, "EMERGENCY", "Emergency"), (r.encrypted, "ENCRYPTED", "Encrypted")] {
            if on {
                fields.push((name, "1".into()));
                comment.push(said.into());
            }
        }
        let start_ms = if r.start_time_ms != 0 { r.start_time_ms } else { r.start_time * 1000 };
        Tags {
            title: if r.talkgroup_tag.is_empty() { r.talkgroup.to_string() } else { r.talkgroup_tag.clone() },
            artist: r.short_name.clone(),
            date: crate::filename::strftime("iso", start_ms, utc_offset_s, false),
            genre: r.talkgroup_group.clone(),
            comment: comment.join(" · "),
            fields,
        }
    }

    /// The named ones, as (ID3 frame, iTunes atom, value); those empty left out.
    fn named(&self) -> impl Iterator<Item = (&'static [u8; 4], &'static [u8; 4], &str)> {
        [(b"TIT2", b"\xa9nam", &self.title), (b"TPE1", b"\xa9ART", &self.artist), (b"TDRC", b"\xa9day", &self.date), (b"TCON", b"\xa9gen", &self.genre), (b"COMM", b"\xa9cmt", &self.comment)]
            .into_iter()
            .filter(|t| !t.2.is_empty())
            .map(|(f, a, v)| (f, a, v.as_str()))
    }

    /// An ID3v2.4 tag, UTF-8 throughout.
    pub fn id3(&self) -> Vec<u8> {
        let mut frames = Vec::new();
        let mut frame = |id: &[u8; 4], body: Vec<u8>| {
            frames.extend_from_slice(id);
            frames.extend_from_slice(&syncsafe(body.len()));
            frames.extend_from_slice(&[0, 0]);
            frames.extend(body);
        };
        for (id, _, v) in self.named() {
            // UTF-8; a comment's language and (empty) description first.
            let mut body = if id == b"COMM" { b"\x03eng\0".to_vec() } else { vec![3] };
            body.extend_from_slice(v.as_bytes());
            frame(id, body);
        }
        for (name, v) in &self.fields {
            let mut body = vec![3];
            body.extend_from_slice(name.as_bytes());
            body.push(0);
            body.extend_from_slice(v.as_bytes());
            frame(b"TXXX", body);
        }
        let mut tag = b"ID3\x04\0\0".to_vec();
        tag.extend_from_slice(&syncsafe(frames.len()));
        tag.extend(frames);
        tag
    }

    /// `wav` (as [`trunk_core::wav::encode`] makes it) with the tag in an `id3 ` chunk after its audio.
    pub fn onto_wav(&self, wav: &mut Vec<u8>) {
        if wav.len() < 12 || &wav[..4] != b"RIFF" {
            return;
        }
        let tag = self.id3();
        let pad = tag.len() % 2;
        wav.extend_from_slice(b"id3 ");
        wav.extend_from_slice(&(tag.len() as u32).to_le_bytes());
        wav.extend(tag);
        wav.resize(wav.len() + pad, 0);
        // (Past 4 GB the size stays "to the end of the file".)
        let riff = u32::from_le_bytes(wav[4..8].try_into().unwrap());
        if riff != u32::MAX {
            let size = u32::try_from(wav.len() - 8).unwrap_or(u32::MAX);
            wav[4..8].copy_from_slice(&size.to_le_bytes());
        }
    }

    /// `mp4` (an M4A) with these as its iTunes tags, replacing any it had
    /// (an encoder's own `©too`). None: not an MP4 laid out as expected.
    pub fn onto_mp4(&self, mp4: &[u8]) -> Option<Vec<u8>> {
        let top = boxes(mp4)?;
        let (moov_at, moov) = top.iter().find(|b| &b.kind == b"moov").map(|b| (b.at, b))?;
        let mdat_at = top.iter().find(|b| &b.kind == b"mdat")?.at;
        let mut body = Vec::new();
        for c in boxes(&mp4[moov.body.clone()])?.iter().filter(|c| &c.kind != b"udta") {
            body.extend_from_slice(&mp4[moov.body.start + c.at..moov.body.start + c.end]);
        }
        body.extend(self.udta());
        let mut new_moov = mp4_box(b"moov", &body);
        // The audio moves by as much as the moov grew, when the moov is before it.
        if moov_at < mdat_at {
            let delta = new_moov.len() as i64 - (moov.end - moov.at) as i64;
            shift_chunks(&mut new_moov[8..], delta)?;
        }
        let mut out = Vec::with_capacity(mp4.len() + new_moov.len());
        out.extend_from_slice(&mp4[..moov.at]);
        out.extend(new_moov);
        out.extend_from_slice(&mp4[moov.end..]);
        Some(out)
    }

    /// `udta` › `meta` › (`hdlr`, `ilst`), as iTunes writes them.
    fn udta(&self) -> Vec<u8> {
        let data = |v: &str| {
            let mut d = 1u32.to_be_bytes().to_vec(); // UTF-8
            d.extend_from_slice(&[0; 4]);
            d.extend_from_slice(v.as_bytes());
            mp4_box(b"data", &d)
        };
        let full = |kind: &[u8; 4], v: &str| {
            let mut d = vec![0; 4];
            d.extend_from_slice(v.as_bytes());
            mp4_box(kind, &d)
        };
        let mut ilst = Vec::new();
        for (_, atom, v) in self.named() {
            ilst.extend(mp4_box(atom, &data(v)));
        }
        for (name, v) in &self.fields {
            ilst.extend(mp4_box(b"----", &[full(b"mean", "com.apple.iTunes"), full(b"name", name), data(v)].concat()));
        }
        let hdlr = mp4_box(b"hdlr", &[&[0u8; 8][..], b"mdirappl", &[0; 9]].concat());
        let meta = mp4_box(b"meta", &[&[0u8; 4][..], &hdlr, &mp4_box(b"ilst", &ilst)].concat());
        mp4_box(b"udta", &meta)
    }
}

fn syncsafe(n: usize) -> [u8; 4] {
    [(n >> 21) as u8 & 0x7f, (n >> 14) as u8 & 0x7f, (n >> 7) as u8 & 0x7f, n as u8 & 0x7f]
}

fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    b.extend_from_slice(kind);
    b.extend_from_slice(body);
    b
}

/// An MP4 box: where it starts and ends, and its body, in what it was read from.
struct Mp4Box {
    kind: [u8; 4],
    at: usize,
    end: usize,
    body: std::ops::Range<usize>,
}

/// The boxes `b` is made of; None when they don't fit it.
fn boxes(b: &[u8]) -> Option<Vec<Mp4Box>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < b.len() {
        let size = u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize;
        let kind: [u8; 4] = b.get(at + 4..at + 8)?.try_into().ok()?;
        let (head, size) = match size {
            0 => (8, b.len() - at),
            1 => (16, u64::from_be_bytes(b.get(at + 8..at + 16)?.try_into().ok()?) as usize),
            s => (8, s),
        };
        if size < head || at + size > b.len() {
            return None;
        }
        out.push(Mp4Box { kind, at, end: at + size, body: at + head..at + size });
        at += size;
    }
    Some(out)
}

/// Every chunk offset (`stco`, `co64`) in a moov's body moved by `delta`.
fn shift_chunks(b: &mut [u8], delta: i64) -> Option<()> {
    for c in boxes(b)? {
        let body = &mut b[c.body.clone()];
        match &c.kind {
            b"trak" | b"mdia" | b"minf" | b"stbl" => shift_chunks(body, delta)?,
            b"stco" | b"co64" => {
                let wide = &c.kind == b"co64";
                let n = u32::from_be_bytes(body.get(4..8)?.try_into().ok()?) as usize;
                let w = if wide { 8 } else { 4 };
                for e in body.get_mut(8..8 + n * w)?.chunks_exact_mut(w) {
                    if wide {
                        let v = u64::from_be_bytes(e.try_into().ok()?) as i64 + delta;
                        e.copy_from_slice(&(v as u64).to_be_bytes());
                    } else {
                        let v = u32::try_from(u32::from_be_bytes(e.try_into().ok()?) as i64 + delta).ok()?;
                        e.copy_from_slice(&v.to_be_bytes());
                    }
                }
            }
            _ => {}
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use trunk_recorder_plugin::SrcEntry;

    fn record() -> CallRecord {
        let src = |src: i64, tag: &str| SrcEntry { src, tag: tag.into(), ..Default::default() };
        CallRecord {
            talkgroup: 101,
            talkgroup_tag: "Fire Disp".into(),
            talkgroup_group: "Fire".into(),
            short_name: "dcfd".into(),
            freq: 851_012_500,
            start_time: 1_764_097_391,
            start_time_ms: 1_764_097_391_250,
            emergency: true,
            src_list: vec![src(1234, "Engine 5"), src(0, ""), src(5678, ""), src(1234, "Engine 5")],
            ..Default::default()
        }
    }

    #[test]
    fn what_a_call_is_tagged_with() {
        let t = Tags::of(&record(), -5 * 3600);
        assert_eq!((t.title.as_str(), t.artist.as_str(), t.date.as_str(), t.genre.as_str()), ("Fire Disp", "dcfd", "2025-11-25T14:03:11", "Fire"));
        assert_eq!(t.comment, "TG 101 · 851.0125 MHz · Units 1234 (Engine 5), 5678 · Emergency");
        let f = |n: &str| t.fields.iter().find(|f| f.0 == n).map(|f| f.1.as_str());
        assert_eq!((f("TALKGROUP"), f("UNITS"), f("FREQUENCY"), f("EMERGENCY"), f("ENCRYPTED")), (Some("101"), Some("1234, 5678"), Some("851012500"), Some("1"), None));
        // No radio heard, no name: no UNITS, the number for a title.
        let t = Tags::of(&CallRecord { src_list: vec![], talkgroup_tag: String::new(), ..record() }, 0);
        assert!(t.fields.iter().all(|f| f.0 != "UNITS") && t.title == "101");
    }

    #[test]
    fn the_wav_carries_an_id3_chunk_after_its_audio() {
        let samples = vec![0.25f32; 801];
        let mut wav = trunk_core::wav::encode(&samples, 8000);
        let audio = wav.clone();
        let t = Tags::of(&record(), 0);
        t.onto_wav(&mut wav);
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize, wav.len() - 8);
        assert_eq!(trunk_core::wav::decode(&wav).unwrap(), trunk_core::wav::decode(&audio).unwrap(), "the audio reads as before");
        let chunk = &wav[audio.len()..];
        assert_eq!(&chunk[..4], b"id3 ");
        let tag = t.id3();
        assert_eq!(&chunk[8..8 + tag.len()], &tag[..]);
        assert_eq!(chunk.len() % 2, 0);
        // A frame: id, syncsafe size, flags, then the UTF-8 text.
        let i = tag.windows(4).position(|w| w == b"TIT2").unwrap();
        assert_eq!(&tag[i + 4..i + 10 + 10], b"\0\0\0\x0a\0\0\x03Fire Disp");
    }

    /// An M4A as an encoder lays it out: ftyp, moov (with a udta of its own) before the audio.
    fn m4a() -> Vec<u8> {
        let trak = |first: u32| {
            let stco = mp4_box(b"stco", &[&[0u8; 4][..], &2u32.to_be_bytes(), &first.to_be_bytes(), &(first + 4).to_be_bytes()].concat());
            mp4_box(b"trak", &mp4_box(b"mdia", &mp4_box(b"minf", &mp4_box(b"stbl", &stco))))
        };
        let (ftyp, own) = (mp4_box(b"ftyp", b"M4A \0\0\0\0"), mp4_box(b"udta", b"encoder's"));
        let first = (ftyp.len() + 8 + trak(0).len() + own.len() + 8) as u32;
        [ftyp, mp4_box(b"moov", &[trak(first), own].concat()), mp4_box(b"mdat", b"AAAABBBB")].concat()
    }

    fn offsets(mp4: &[u8]) -> Vec<u32> {
        let i = mp4.windows(4).position(|w| w == b"stco").unwrap() + 4;
        let n = u32::from_be_bytes(mp4[i + 4..i + 8].try_into().unwrap()) as usize;
        (0..n).map(|k| u32::from_be_bytes(mp4[i + 8 + 4 * k..i + 12 + 4 * k].try_into().unwrap())).collect()
    }

    #[test]
    fn the_m4a_gets_itunes_tags_and_its_audio_is_still_found() {
        let before = m4a();
        let after = Tags::of(&record(), 0).onto_mp4(&before).unwrap();
        // The chunk offsets still point at the audio.
        for (o, want) in offsets(&after).into_iter().zip([&b"AAAA"[..], b"BBBB"]) {
            assert_eq!(&after[o as usize..o as usize + 4], want);
        }
        let has = |s: &[u8]| after.windows(s.len()).any(|w| w == s);
        assert!(has(b"\xa9nam") && has(b"Fire Disp") && has(b"mdirappl") && has(b"com.apple.iTunes") && has(b"UNITS") && has(b"1234, 5678"));
        assert!(!has(b"encoder's"), "the encoder's udta is replaced");
        assert!(boxes(&after).is_some() && Tags::default().onto_mp4(b"not an mp4").is_none());
    }

    /// A real encoder's file, when the machine has ffmpeg, reads back with ffprobe.
    #[test]
    fn ffprobe_reads_the_tags() {
        let Some(ffmpeg) = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"].into_iter().find(|p| std::path::Path::new(p).exists()) else { return };
        let ffprobe = ffmpeg.replace("ffmpeg", "ffprobe");
        let dir = std::env::temp_dir().join(format!("trunk-tags-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = Tags::of(&record(), 0);
        let mut wav = trunk_core::wav::encode(&vec![0.1f32; 16000], 8000);
        t.onto_wav(&mut wav);
        let (w, m) = (dir.join("a.wav"), dir.join("a.m4a"));
        std::fs::write(&w, &wav).unwrap();
        let ok = std::process::Command::new(ffmpeg).args(["-y", "-loglevel", "error", "-i"]).arg(&w).args(["-c:a", "aac", "-movflags", "+faststart"]).arg(&m).status().unwrap();
        assert!(ok.success());
        std::fs::write(&m, t.onto_mp4(&std::fs::read(&m).unwrap()).unwrap()).unwrap();
        for f in [&w, &m] {
            let out = std::process::Command::new(&ffprobe).args(["-v", "error", "-show_entries", "format_tags:format=duration", "-of", "json"]).arg(f).output().unwrap();
            let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            let tags = &j["format"]["tags"];
            let get = |k: &str| tags.as_object().unwrap().iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str().unwrap().to_string());
            assert_eq!(get("title").as_deref(), Some("Fire Disp"), "{f:?}: {j}");
            assert_eq!(get("artist").as_deref(), Some("dcfd"), "{f:?}: {j}");
            assert_eq!(get("UNITS").as_deref(), Some("1234, 5678"), "{f:?}: {j}");
            let d: f64 = j["format"]["duration"].as_str().unwrap().parse().unwrap();
            assert!((d - 2.0).abs() < 0.1, "{f:?}: {d}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
