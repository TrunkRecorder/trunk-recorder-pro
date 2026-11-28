//! Call products: what a plugin worked out from a call. A transcriber makes a [`Transcript`]
//! of its audio; a data decoder on a tap makes a [`Page`] or an [`AcarsMessage`] of what the
//! channel carried. A plugin sends one with [`crate::PluginMessage::CallProduct`]
//! ([`crate::Host::product`]); the recorder keeps it with the call (its store, and a
//! `.products.jsonl` beside the call's JSON), shows it on the call's page, and sends it on to
//! the plugins subscribed to its topic ([`ProductData::topic`]: `product.<kind>`).
//!
//! A call has as many products as were made of it: two transcribers' transcripts, every page
//! in a pager transmission. None replaces another.
//!
//! Adding a kind: its type and a [`ProductData`] variant here. A kind made by a plugin the
//! recorder has no type for is [`ProductData::Other`]: kept, searched and sent on all the same.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// The topic prefix of the products: a plugin subscribes to `product.<kind>`
/// ([`ProductData::topic`]), or to [`crate::topic::PRODUCT`] for every kind.
pub const PRODUCT_TOPIC: &str = "product.";

/// Something a plugin made of a call.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Product {
    /// The call it's of, by [`crate::CallRecord::transmission_id`]: what a plugin that was
    /// handed the concluded call says. Always set on a product the recorder sends on.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub transmission_id: String,
    /// The call it's of while it still runs, by [`crate::TapChunk::call_id`] (and
    /// [`crate::CallInfo`]'s `id`): what a decoder on a tap says, the call having no
    /// transmission id yet. The recorder keeps the product until the call concludes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<u32>,
    /// The plugin that made it, by [`crate::Manifest::id`]. The recorder sets it: a plugin
    /// leaves it empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    /// Unix seconds: when what it holds was heard (a page), or when it was made (a transcript).
    pub time: f64,
    /// The call's system, talkgroup and frequency, on a product the recorder sends on. From a
    /// plugin, only its `path` is read: where the call is filed, for one concluded long ago
    /// ([`Product::of_call`] says it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<ProductCall>,
    #[serde(flatten)]
    pub data: ProductData,
}

/// The call a product is of, in brief: what a plugin taking products alone (an alert on a
/// page) needs of it. The whole call is [`crate::ConcludedCall`]'s, by the transmission id.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProductCall {
    /// [`crate::SystemInfo::id`]: the system's identity.
    pub system_id: u32,
    pub short_name: String,
    pub talkgroup: u32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub talkgroup_tag: String,
    pub freq_hz: f64,
    /// [`crate::ConcludedCall::path`]; empty for a call with no files.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub path: String,
}

impl Product {
    /// `data`, of the concluded call `transmission_id` names, made now.
    pub fn of(transmission_id: impl Into<String>, data: impl Into<ProductData>) -> Product {
        Product { transmission_id: transmission_id.into(), time: now(), data: data.into(), ..Product::default() }
    }

    /// `data`, of concluded call `call` (a [`crate::ConcludedCall`]'s, or one of
    /// [`crate::Host::calls`]) filed under `path`, made now. For any call the recorder still
    /// has, however old.
    pub fn of_call(path: &str, call: &crate::CallRecord, data: impl Into<ProductData>) -> Product {
        let filed = ProductCall { path: path.to_string(), ..ProductCall::default() };
        Product { call: Some(filed), ..Product::of(call.transmission_id.as_str(), data) }
    }

    /// `data`, of the running call `call_id`, heard at `time` (Unix seconds).
    pub fn live(call_id: u32, time: f64, data: impl Into<ProductData>) -> Product {
        Product { call_id: Some(call_id), time, data: data.into(), ..Product::default() }
    }

    /// `transcript`, `page`, ….
    pub fn kind(&self) -> &str {
        self.data.kind()
    }

    /// Its text ([`ProductData::text`]).
    pub fn text(&self) -> &str {
        self.data.text()
    }
}

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// What a product is. In JSON, `"kind"` and `"data"` side by side:
/// `{"kind": "page", "data": {"protocol": "pocsag", "address": 1234567, "text": "…"}}`.
#[derive(Clone, Debug, PartialEq)]
pub enum ProductData {
    Transcript(Transcript),
    Page(Page),
    Acars(AcarsMessage),
    /// A kind this build has no type for, as it came. Its `data.text`, when it has one, is
    /// its text.
    Other { kind: String, data: Value },
}

impl Default for ProductData {
    fn default() -> Self {
        ProductData::Other { kind: String::new(), data: Value::Null }
    }
}

/// The kinds with a type: `(kind, what it is)`.
pub const PRODUCT_KINDS: &[(&str, &str)] = &[
    (Transcript::KIND, "What was said in a call's audio, as text"),
    (Page::KIND, "A pager message (POCSAG, FLEX)"),
    (AcarsMessage::KIND, "An aircraft's ACARS message"),
];

impl ProductData {
    pub fn kind(&self) -> &str {
        match self {
            ProductData::Transcript(_) => Transcript::KIND,
            ProductData::Page(_) => Page::KIND,
            ProductData::Acars(_) => AcarsMessage::KIND,
            ProductData::Other { kind, .. } => kind,
        }
    }

    /// The topic it goes to plugins on: `product.<kind>`.
    pub fn topic(&self) -> String {
        format!("{PRODUCT_TOPIC}{}", self.kind())
    }

    /// What it says, plainly: what a list shows and a search looks in.
    pub fn text(&self) -> &str {
        match self {
            ProductData::Transcript(t) => &t.text,
            ProductData::Page(p) => &p.text,
            ProductData::Acars(a) => &a.text,
            ProductData::Other { data, .. } => data.get("text").and_then(Value::as_str).unwrap_or(""),
        }
    }

    /// Its `data` alone, as JSON (what a store keeps beside the kind).
    pub fn value(&self) -> Value {
        let v = match self {
            ProductData::Transcript(t) => serde_json::to_value(t),
            ProductData::Page(p) => serde_json::to_value(p),
            ProductData::Acars(a) => serde_json::to_value(a),
            ProductData::Other { data, .. } => return data.clone(),
        };
        v.unwrap_or(Value::Null)
    }

    /// The product of `kind` that `data` holds: its type where the kind has one and `data`
    /// is one, else [`ProductData::Other`].
    pub fn from_value(kind: &str, data: Value) -> ProductData {
        fn typed<T: serde::de::DeserializeOwned>(data: &Value) -> Option<T> {
            T::deserialize(data).ok()
        }
        let known = match kind {
            Transcript::KIND => typed(&data).map(ProductData::Transcript),
            Page::KIND => typed(&data).map(ProductData::Page),
            AcarsMessage::KIND => typed(&data).map(ProductData::Acars),
            _ => None,
        };
        known.unwrap_or_else(|| ProductData::Other { kind: kind.to_string(), data })
    }
}

/// `{"kind", "data"}`: how a [`ProductData`] is written.
#[derive(Serialize, Deserialize)]
struct Tagged {
    kind: String,
    #[serde(default)]
    data: Value,
}

impl Serialize for ProductData {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        Tagged { kind: self.kind().to_string(), data: self.value() }.serialize(s)
    }
}

impl<'de> Deserialize<'de> for ProductData {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let t = Tagged::deserialize(d)?;
        Ok(ProductData::from_value(&t.kind, t.data))
    }
}

impl From<Transcript> for ProductData {
    fn from(t: Transcript) -> Self {
        ProductData::Transcript(t)
    }
}
impl From<Page> for ProductData {
    fn from(p: Page) -> Self {
        ProductData::Page(p)
    }
}
impl From<AcarsMessage> for ProductData {
    fn from(a: AcarsMessage) -> Self {
        ProductData::Acars(a)
    }
}

/// What was said in a call's audio.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transcript {
    /// All of it, as one text.
    pub text: String,
    /// Its language, as BCP 47 has it ("en", "es-MX"); empty when not known.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub language: String,
    /// What made it ("whisper-large-v3"); empty when not said.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// The text in pieces, each with its place in the call's audio; none when the
    /// transcriber gives only the whole.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<Segment>,
}

/// A piece of a [`Transcript`]. Its times are from the start of the call's audio file, ms: as
/// [`crate::SrcEntry::pos`] places the radios in it, so a segment's speaker is the radio
/// talking then.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Segment {
    pub start_ms: u32,
    pub end_ms: u32,
    pub text: String,
    /// The radio talking, where the transcriber has matched it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<u32>,
    /// How sure the transcriber is of it, 0–1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

impl Transcript {
    pub const KIND: &'static str = "transcript";
}

/// What a page carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageFormat {
    /// Text.
    #[default]
    Alpha,
    /// Digits (and the few signs a numeric pager shows).
    Numeric,
    /// No message: the pager only sounds.
    Tone,
    /// Data that isn't text, as hex.
    Binary,
}

/// A pager message.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Page {
    /// "pocsag" | "flex"
    pub protocol: String,
    /// Symbols a second: POCSAG's 512, 1200 or 2400; FLEX's 1600, 3200 or 6400.
    pub baud: u32,
    /// The pager it's for: POCSAG's RIC, FLEX's capcode.
    pub address: u64,
    /// POCSAG's function bits, 0–3 (which of a pager's four alerts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<u8>,
    pub format: PageFormat,
    /// The message; empty for a tone-only page.
    pub text: String,
    /// Bit errors corrected in its codewords.
    #[serde(skip_serializing_if = "is_zero")]
    pub errors: u32,
}

impl Page {
    pub const KIND: &'static str = "page";
}

/// An ACARS message: one block, as an aircraft or a ground station sent it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AcarsMessage {
    /// The mode character ("2": VHF category A).
    pub mode: String,
    /// The aircraft's registration, as sent, without its leading dots ("N12345").
    pub registration: String,
    /// The flight ("UA0123"); a downlink names it, an uplink doesn't.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub flight: String,
    /// What kind of message: two characters ("H1", "Q0", "_d").
    pub label: String,
    /// The block's id: a digit from an aircraft, a letter from the ground.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub block_id: String,
    /// The block it acknowledges; empty for none (NAK).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub ack: String,
    /// A downlink's message number ("M01A").
    #[serde(skip_serializing_if = "String::is_empty")]
    pub message_no: String,
    pub text: String,
    /// More blocks of the message follow (it ended with ETB).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub more: bool,
    /// Characters whose parity was wrong and put right.
    #[serde(skip_serializing_if = "is_zero")]
    pub errors: u32,
}

impl AcarsMessage {
    pub const KIND: &'static str = "acars";
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// The kind a product topic names (`product.page` → `page`), if it is one.
pub fn kind_of_topic(topic: &str) -> Option<&str> {
    topic.strip_prefix(PRODUCT_TOPIC).filter(|k| !k.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kind_and_data_sit_side_by_side() {
        let page = Page { protocol: "pocsag".into(), baud: 1200, address: 1234567, function: Some(3), format: PageFormat::Alpha, text: "ENGINE 4 RESPOND".into(), errors: 0 };
        let p = Product::live(7, 1_700_000_000.5, page.clone());
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v, json!({"call_id": 7, "time": 1_700_000_000.5, "kind": "page", "data": {"protocol": "pocsag", "baud": 1200, "address": 1234567, "function": 3, "format": "alpha", "text": "ENGINE 4 RESPOND"}}));
        let back: Product = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
        assert_eq!((back.kind(), back.text(), back.data.topic().as_str()), ("page", "ENGINE 4 RESPOND", "product.page"));
        assert_eq!(back.data, ProductData::Page(page));
    }

    #[test]
    fn every_known_kind_reads_back_as_its_type() {
        let all: [ProductData; 3] = [
            Transcript { text: "engine four on scene".into(), language: "en".into(), model: String::new(), segments: vec![Segment { start_ms: 0, end_ms: 1800, text: "engine four on scene".into(), unit: Some(4101), confidence: Some(0.9) }] }.into(),
            Page { protocol: "flex".into(), baud: 1600, address: 9, ..Page::default() }.into(),
            AcarsMessage { mode: "2".into(), registration: "N12345".into(), label: "H1".into(), text: "POS".into(), ..AcarsMessage::default() }.into(),
        ];
        assert_eq!(all.iter().map(ProductData::kind).collect::<Vec<_>>(), PRODUCT_KINDS.iter().map(|k| k.0).collect::<Vec<_>>(), "each listed, in order");
        for d in all {
            let p = Product::of("dc-101-7-1000", d.clone());
            let back: Product = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
            assert_eq!(back.data, d);
            assert_eq!(ProductData::from_value(d.kind(), d.value()), d, "as a store keeps it");
        }
    }

    #[test]
    fn a_kind_without_a_type_is_kept_as_it_came() {
        let v = json!({"transmission_id": "dc-101-7-1000", "time": 1.0, "kind": "aprs", "data": {"from": "N0CALL", "text": "hello"}});
        let p: Product = serde_json::from_value(v.clone()).unwrap();
        assert_eq!((p.kind(), p.text()), ("aprs", "hello"));
        assert_eq!(serde_json::to_value(&p).unwrap(), v);
        // And a known kind whose data isn't its type's: kept, not refused.
        let odd: Product = serde_json::from_value(json!({"time": 1.0, "kind": "page", "data": {"address": "not a number"}})).unwrap();
        assert!(matches!(odd.data, ProductData::Other { .. }));
        assert_eq!(odd.kind(), "page");
    }

    #[test]
    fn topics_name_kinds() {
        assert_eq!(kind_of_topic("product.page"), Some("page"));
        assert_eq!(kind_of_topic("product."), None);
        assert_eq!(kind_of_topic("tap.fm.discriminator"), None);
    }
}
