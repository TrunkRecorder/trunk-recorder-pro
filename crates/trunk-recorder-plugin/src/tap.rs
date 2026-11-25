//! Signal taps: what a voice channel's receive chain holds part-way through, handed out as it
//! is made. A tap is a named place in the chain, after the channel's IQ and before the call's
//! audio: the FM discriminator's output, a voice frame before decryption, each frame's error
//! count. Every tap gives the same thing, a stream of [`TapChunk`]s: its data (samples,
//! frames, or none) and the notes about it, one stream for each transmission on a channel.
//! The recorder saves a stream to files and sends its chunks to the plugins subscribed to the
//! tap's topic ([`Tap::topic`]), both from the one [`TapChunk`].
//!
//! Adding a tap: a [`Tap`] and its [`TapInfo`] here, and the place in the recorder's decoder
//! that makes its pieces. Routing, files, the plugin topic and the settings follow from the list.

use crate::protocol::{base64, bits_hex, hex_bits, Codec, FrameKind};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The topic prefix of the taps: a plugin subscribes to `tap.<id>` ([`Tap::topic`]).
pub const TAP_TOPIC: &str = "tap.";

/// A place in a voice channel's receive chain that can be tapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tap {
    /// The FM discriminator's output, before de-emphasis and the voice filters: what a data
    /// decoder (POCSAG, FLEX, a trunking control channel) takes. Notes: none.
    #[serde(rename = "fm.discriminator")]
    FmDiscriminator,
    /// Digital voice frames as they came off the air (P25 Phase 1 and 2, DMR, NXDN, EDACS
    /// ProVoice): error-corrected, not decrypted, whether the recorder can play them or not.
    /// Notes: an [`AirFrame`] for each.
    #[serde(rename = "voice.air")]
    VoiceAir,
    /// How well each voice frame decoded: no data, a [`FrameErrors`] note for each frame vocoded.
    #[serde(rename = "voice.errors")]
    VoiceErrors,
}

/// What a tap's chunks carry ([`TapData`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Carries {
    /// Samples, saved as a WAV file.
    Samples,
    /// Vocoder frames, saved a line each.
    Frames,
    /// Nothing but notes.
    Notes,
}

/// A tap, described: the settings' list and the documentation are made of these.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct TapInfo {
    pub tap: Tap,
    /// As configs, topics and file names have it: `<stage>.<what>`.
    pub id: &'static str,
    pub title: &'static str,
    /// One sentence: what comes out, and what it's for.
    pub about: &'static str,
    /// The channels it comes from, in a word or two.
    pub from: &'static str,
    pub carries: Carries,
}

/// Every tap, in the order of the receive chain.
pub const TAPS: &[TapInfo] = &[
    TapInfo {
        tap: Tap::FmDiscriminator,
        id: "fm.discriminator",
        title: "FM discriminator",
        about: "The FM demodulator's output before de-emphasis and the voice filters, 16-bit at 22,050 Hz while the carrier is up: for data decoders such as multimon-ng (POCSAG, FLEX).",
        from: "Analog FM",
        carries: Carries::Samples,
    },
    TapInfo {
        tap: Tap::VoiceAir,
        id: "voice.air",
        title: "Voice frames off the air",
        about: "Each digital voice frame (P25, DMR, NXDN, ProVoice) after error correction and before decryption, with its place in the superframe, its errors and the encryption it is under.",
        from: "Digital voice",
        carries: Carries::Frames,
    },
    TapInfo {
        tap: Tap::VoiceErrors,
        id: "voice.errors",
        title: "Voice frame errors",
        about: "The bit errors corrected in each voice frame, and what the vocoder made of it: no audio, only the figures.",
        from: "Digital voice",
        carries: Carries::Notes,
    },
];

impl Tap {
    pub fn info(self) -> &'static TapInfo {
        TAPS.iter().find(|i| i.tap == self).expect("every tap is listed")
    }

    /// `fm.discriminator`, …
    pub fn id(self) -> &'static str {
        self.info().id
    }

    pub fn from_id(id: &str) -> Option<Tap> {
        TAPS.iter().find(|i| i.id == id).map(|i| i.tap)
    }

    /// The topic its chunks go to plugins on: `tap.<id>`.
    pub fn topic(self) -> String {
        format!("{TAP_TOPIC}{}", self.id())
    }

    /// The tap topic `topic` names, if it is one.
    pub fn of_topic(topic: &str) -> Option<Tap> {
        topic.strip_prefix(TAP_TOPIC).and_then(Tap::from_id)
    }

    fn bit(self) -> u32 {
        1 << TAPS.iter().position(|i| i.tap == self).expect("every tap is listed")
    }
}

impl std::fmt::Display for Tap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

/// Some of the taps: those turned on for a channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TapSet(u32);

impl TapSet {
    pub const NONE: TapSet = TapSet(0);

    pub fn of(taps: impl IntoIterator<Item = Tap>) -> TapSet {
        taps.into_iter().fold(TapSet::NONE, TapSet::with)
    }

    /// The taps the topics in `topics` name.
    pub fn of_topics<'a>(topics: impl IntoIterator<Item = &'a str>) -> TapSet {
        TapSet::of(topics.into_iter().filter_map(Tap::of_topic))
    }

    #[must_use]
    pub fn with(self, tap: Tap) -> TapSet {
        TapSet(self.0 | tap.bit())
    }

    #[must_use]
    pub fn and(self, other: TapSet) -> TapSet {
        TapSet(self.0 | other.0)
    }

    #[inline]
    pub fn has(self, tap: Tap) -> bool {
        self.0 & tap.bit() != 0
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = Tap> {
        TAPS.iter().map(|i| i.tap).filter(move |&t| self.has(t))
    }
}

/// What a chunk carries, apart from its notes. In JSON: `{"kind": "none"}`,
/// `{"kind": "pcm", "rate": 22050, "pcm": "<base64 of 16-bit little-endian samples>"}`,
/// `{"kind": "frames", "codec": "imbe", "frames": ["<hex>", …]}` (a frame's bits as
/// [`bits_hex`] has them).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TapData {
    /// Notes only.
    #[default]
    None,
    /// Mono samples at `rate` Hz.
    Pcm {
        rate: u32,
        #[serde(rename = "pcm", with = "pcm_b64")]
        samples: Vec<i16>,
    },
    /// Vocoder frames: each its bits (0 / 1), [`Codec::bits`] of them.
    Frames {
        codec: Codec,
        #[serde(with = "frames_hex")]
        frames: Vec<Vec<u8>>,
    },
}

impl TapData {
    /// How many samples or frames it holds.
    pub fn len(&self) -> usize {
        match self {
            TapData::None => 0,
            TapData::Pcm { samples, .. } => samples.len(),
            TapData::Frames { frames, .. } => frames.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The same kind of data (its rate, its codec) with nothing in it: what a stream's files say of it.
    pub fn format(&self) -> TapData {
        match self {
            TapData::None => TapData::None,
            TapData::Pcm { rate, .. } => TapData::Pcm { rate: *rate, samples: Vec::new() },
            TapData::Frames { codec, .. } => TapData::Frames { codec: *codec, frames: Vec::new() },
        }
    }

    /// `more` after it: true when it's the same kind and went on (data of another kind doesn't).
    pub fn append(&mut self, more: TapData) -> bool {
        match (self, more) {
            (TapData::None, TapData::None) => true,
            (TapData::Pcm { rate, samples }, TapData::Pcm { rate: r, samples: mut s }) if *rate == r => {
                samples.append(&mut s);
                true
            }
            (TapData::Frames { codec, frames }, TapData::Frames { codec: c, frames: mut f }) if *codec == c => {
                frames.append(&mut f);
                true
            }
            _ => false,
        }
    }
}

mod pcm_b64 {
    use super::base64;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(samples: &[i16], s: S) -> Result<S::Ok, S::Error> {
        let bytes: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        s.serialize_str(&base64::encode(&bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<i16>, D::Error> {
        let text = String::deserialize(d)?;
        Ok(base64::decode(&text).chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect())
    }
}

mod frames_hex {
    use super::{bits_hex, hex_bits};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(frames: &[Vec<u8>], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(frames.iter().map(|f| bits_hex(f)))
    }

    /// (Whole bytes back: a frame's last byte is zero-padded, and [`super::Codec::bits`] says where it ends.)
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Vec<u8>>, D::Error> {
        let frames = Vec::<String>::deserialize(d)?;
        frames.iter().map(|f| hex_bits(f, f.len() * 4).ok_or_else(|| serde::de::Error::custom("a frame's bits aren't hex"))).collect()
    }
}

/// Something said about a chunk's data: `at` is the sample or frame it's about, counted from
/// the chunk's first; the rest are the fields of its tap's note type ([`AirFrame`],
/// [`FrameErrors`]), flat beside it. A chunk with no data counts its notes instead.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TapNote {
    pub at: u32,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

impl TapNote {
    /// `fields` (a struct of named fields) about item `at`.
    pub fn of(at: u32, fields: &impl Serialize) -> TapNote {
        let fields = match serde_json::to_value(fields) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        };
        TapNote { at, fields }
    }

    /// Its fields as `T`; None when they aren't one.
    pub fn get<T: DeserializeOwned>(&self) -> Option<T> {
        serde_json::from_value(Value::Object(self.fields.clone())).ok()
    }
}

/// [`Tap::VoiceAir`]'s note: a digital voice frame as it was sent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AirFrame {
    /// What it was sent as, named as [`crate::CallInfo::voice_mode`] names it: "p25" |
    /// "p25_phase2" | "dmr" | "nxdn48" | "nxdn96" | "provoice". `n` and the encryption fields
    /// are that protocol's.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub voice_mode: String,
    /// Its place in the superframe. P25 Phase 1: 0–17 (LDU1's nine frames, then LDU2's).
    /// Phase 2: 0–17 (four 4V bursts, then the 2V). DMR: 0–17 (bursts A–F, three frames
    /// each). NXDN: 0–15 (four frames of four; a 9600 baud FACCH1 takes two at a time).
    /// ProVoice: 0–3 (its frame's four). None where the count isn't known: a lost burst, a
    /// transmission joined late, an NXDN frame whose SACCH didn't decode or (Type-D) has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u8>,
    /// Bit errors the FEC corrected in the frame.
    pub errs: u32,
    /// IMBE: those in u0, the word carrying the pitch. AMBE: 0.
    pub e0: u32,
    /// Known lost (a cut LDU): its bits mean nothing.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub erased: bool,
    /// Whether the transmission is encrypted, as far as was known when the frame came: left
    /// out until a header, link control or an encryption sync has said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<bool>,
    /// The encryption the frame is under, once the air has said: the algorithm, the key and
    /// the message indicator or initialization vector (hex). P25: the encryption sync's ALGID,
    /// KID and MI (9 bytes). DMR: the privacy indicator header's algorithm, key and IV (4
    /// bytes); basic privacy sends none. NXDN: the VCALL's cipher type (1 scrambler, 2 DES,
    /// 3 AES) and key, and the VCALL_IV's IV (8 bytes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub algid: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyid: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mi: Option<String>,
}

impl AirFrame {
    /// `bytes` as [`AirFrame::mi`] has them.
    pub fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// [`Tap::VoiceErrors`]'s note: how one voice frame decoded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameErrors {
    pub codec: Codec,
    /// Bit errors the FEC corrected in the frame.
    pub errs: u32,
    /// IMBE: those in u0. AMBE: 0.
    pub e0: u32,
    /// Known lost; `errs` is then meaningless.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub erased: bool,
    /// What the vocoder made of it.
    pub kind: FrameKind,
}

/// A piece of a tap's stream ([`crate::HostMessage::Tap`], on topic [`Tap::topic`]), and the
/// first line of the stream's saved notes.
///
/// A stream is one transmission on one channel (a TDMA channel: on one slot), from the first
/// the tap makes of it to its end: the air's terminator, half a second with nothing more, or
/// the channel closing. Its chunks come in order, `seq` counting them from 0; `first` and
/// `last` mark its ends (the last may carry nothing).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TapChunk {
    pub tap: Tap,
    /// The stream it's a piece of: a number no other stream of this run has.
    pub stream: u64,
    pub seq: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub first: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub last: bool,
    /// Where in the stream its data starts: samples, frames, or (a tap with no data) notes
    /// before it. A note's place in the stream is this plus its `at`.
    pub offset: u64,
    /// [`crate::SystemInfo::index`]: for this run only.
    pub system: u16,
    /// [`crate::SystemInfo::id`]: the system's identity.
    pub system_id: u32,
    pub short_name: String,
    /// The channel's frequency, Hz.
    pub freq_hz: f64,
    /// Its TDMA slot (0 where there's one).
    #[serde(default)]
    pub slot: u8,
    /// The call on the channel as the chunk was made, if there was one (a tap doesn't wait
    /// for a call: voice before its grant, or data no call is made of, comes without).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub talkgroup: Option<u32>,
    /// Unix seconds when the recorder handed it out (its data is from just before).
    pub time: f64,
    pub data: TapData,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<TapNote>,
}

impl TapChunk {
    /// Its notes as its tap's note type, each with its place in the stream; those that aren't one, left out.
    pub fn notes_as<T: DeserializeOwned>(&self) -> Vec<(u64, T)> {
        self.notes.iter().filter_map(|n| Some((self.offset + n.at as u64, n.get()?))).collect()
    }

    /// How many items (samples, frames, or notes where there's no data) it adds to its stream.
    pub fn items(&self) -> usize {
        if self.data == TapData::None { self.notes.len() } else { self.data.len() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_the_serde_names_and_topics() {
        for i in TAPS {
            assert_eq!(serde_json::to_value(i.tap).unwrap(), i.id);
            assert_eq!(Tap::from_id(i.id), Some(i.tap));
            assert_eq!(Tap::of_topic(&i.tap.topic()), Some(i.tap));
            assert!(i.id.contains('.') && !i.id.contains("raw"), "{}: <stage>.<what>", i.id);
        }
        assert_eq!(Tap::of_topic("voice"), None);
        assert_eq!(Tap::of_topic("tap.nothing"), None);
        let set = TapSet::of_topics(["voice", "tap.voice.air"]);
        assert!(set.has(Tap::VoiceAir) && !set.has(Tap::FmDiscriminator));
        assert_eq!(set.iter().collect::<Vec<_>>(), [Tap::VoiceAir]);
        assert!(TapSet::NONE.is_empty());
    }

    #[test]
    fn a_chunk_round_trips_with_its_data_and_notes() {
        let frame: Vec<u8> = (0..88).map(|i| (i % 3 == 0) as u8).collect();
        let air = AirFrame { voice_mode: "p25".into(), n: Some(9), errs: 2, encrypted: Some(true), algid: Some(0x84), keyid: Some(1), mi: Some("00".repeat(9)), ..Default::default() };
        let chunk = TapChunk {
            tap: Tap::VoiceAir,
            stream: 7,
            seq: 1,
            first: false,
            last: true,
            offset: 9,
            system: 0,
            system_id: 3,
            short_name: "dcfd".into(),
            freq_hz: 857_237_500.0,
            slot: 0,
            call_id: Some(4),
            talkgroup: Some(101),
            time: 1.75e9,
            data: TapData::Frames { codec: Codec::Imbe, frames: vec![frame.clone()] },
            notes: vec![TapNote::of(0, &air)],
        };
        let v = serde_json::to_value(&chunk).unwrap();
        assert_eq!(v["tap"], "voice.air");
        assert_eq!(v["data"]["kind"], "frames");
        assert_eq!(v["notes"][0]["at"], 0);
        assert_eq!(v["notes"][0]["algid"], 0x84);
        let back: TapChunk = serde_json::from_value(v).unwrap();
        let TapData::Frames { frames, .. } = &back.data else { panic!("frames") };
        assert_eq!(frames[0][..88], frame[..]);
        assert_eq!(back.notes_as::<AirFrame>(), [(9, air)]);

        let pcm = TapData::Pcm { rate: 22_050, samples: vec![-3, 0, 32_767] };
        let v = serde_json::to_value(&pcm).unwrap();
        assert_eq!(v["kind"], "pcm");
        assert_eq!(serde_json::from_value::<TapData>(v).unwrap(), pcm);
        assert_eq!(serde_json::to_value(TapData::None).unwrap(), serde_json::json!({ "kind": "none" }));
    }
}
