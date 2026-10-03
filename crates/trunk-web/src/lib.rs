//! Trunk Recorder Pro in the browser. Exposes to the engine worker
//! (web/src/web/engine.worker.ts):
//!
//! * [`WebSession`] — the shared recording session (trunk-app): feed it u8
//!   IQ, poll it for interface messages, live audio and call files.
//! * [`WebRtl`] — an RTL-SDR dongle over WebUSB (rtlsdr-nusb on nusb's WebUSB
//!   backend; works in a worker once the page has been granted the device).
//! * [`WebSurvey`] — the first-run survey (trunk-app): it asks for retunes,
//!   takes u8 IQ, and reports `survey` / `surveySpectrum` messages.
//! * [`WebProfiler`] — a source's roll-off for Setup: takes u8 IQ, answers
//!   with a `sourceProfile` message.
//!
//! Build: `cargo build -p trunk-web --target wasm32-unknown-unknown --profile
//! dist` with `-C target-feature=+simd128`, then `wasm-bindgen --target web`
//! (see web/package.json `build:wasm`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::{Array, Object, Reflect, Uint8Array};
use rtlsdr_nusb::{Device, GainConfig, RawIq, RxStream};
use trunk_app::survey::{Request, SurveySession};
use trunk_app::stats::{History, Topics};
use trunk_app::{Config, Output, Session};
use trunk_core::survey::Command;
use wasm_bindgen::prelude::*;

fn local_offset(t: i64) -> i32 {
    let d = js_sys::Date::new(&JsValue::from_f64(t as f64 * 1000.0));
    -(d.get_timezone_offset() as i32) * 60
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn console_error(s: &str);
    #[wasm_bindgen(js_name = setTimeout)]
    fn set_timeout(f: &js_sys::Function, ms: i32);
}

/// A panic aborts as "unreachable"; say what it was first.
#[wasm_bindgen(start)]
fn start() {
    std::panic::set_hook(Box::new(|info| console_error(&format!("trunk-web panicked: {info}"))));
}

async fn sleep_ms(ms: i32) {
    let p = js_sys::Promise::new(&mut |resolve, _| set_timeout(&resolve, ms));
    let _ = wasm_bindgen_futures::JsFuture::from(p).await;
}

fn set(o: &Object, k: &str, v: impl Into<JsValue>) {
    let _ = Reflect::set(o, &JsValue::from_str(k), &v.into());
}

/// `history`: where the session's minutes go (the dashboard's charts).
fn to_js(out: &mut Vec<Output>, history: &mut History) -> Array {
    let a = Array::new();
    for o in out.drain(..) {
        let obj = Object::new();
        match o {
            // (No plugins in the browser; none are asked for.)
            Output::Plugin(_) | Output::Log(_) => continue,
            Output::Rollup(r) => {
                history.insert(&r);
                continue;
            }
            // (One page, which asked for its topics.)
            Output::Text(t) | Output::Topic { text: t, .. } => {
                set(&obj, "t", "text");
                set(&obj, "json", t);
            }
            Output::Audio { system, short_name, tg, frame } => {
                set(&obj, "t", "audio");
                set(&obj, "system", system);
                set(&obj, "shortName", short_name);
                set(&obj, "tg", tg);
                set(&obj, "frame", Uint8Array::from(frame.as_slice()));
            }
            // (No frame capture in the browser: the setting is desktop-only.)
            Output::File { rel, wav, json, entry, .. } => {
                set(&obj, "t", "file");
                set(&obj, "rel", rel);
                set(&obj, "wav", Uint8Array::from(wav.as_slice()));
                set(&obj, "json", json);
                set(&obj, "entry", entry.to_string());
            }
        }
        a.push(&obj);
    }
    a
}

#[wasm_bindgen]
pub struct WebSession {
    s: Session,
    out: Vec<Output>,
    /// This session's minutes (the browser keeps no files of them).
    history: History,
}

#[wasm_bindgen]
impl WebSession {
    /// `config_json`: the interface's Config. `bandplans_json`: `{shortName: plan}`
    /// saved from a previous run ([`WebSession::bandplans`]).
    #[wasm_bindgen(constructor)]
    pub fn new(config_json: &str, epoch_ms: f64, bandplans_json: Option<String>) -> Result<WebSession, JsError> {
        let cfg: Config = serde_json::from_str(config_json).map_err(|e| JsError::new(&format!("config: {e}")))?;
        let plans: serde_json::Map<String, serde_json::Value> = bandplans_json.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
        let plan = |name: &str| plans.get(name).and_then(|v| v.as_str()).map(str::to_string);
        let s = Session::new(cfg, epoch_ms, &plan, local_offset).map_err(|e| JsError::new(&e))?;
        Ok(WebSession { s, out: Vec::new(), history: History::default() })
    }
    /// What the page watches: JSON `["spectrum:0", "log", …]`.
    pub fn set_topics(&mut self, topics_json: &str) {
        let t: Vec<String> = serde_json::from_str(topics_json).unwrap_or_default();
        self.s.set_topics(Topics::new(t));
    }
    /// A `statsQuery` (JSON) → its `statsResult` (JSON), from this session's minutes.
    pub fn stats_query(&self, query_json: &str) -> String {
        let q: serde_json::Value = serde_json::from_str(query_json).unwrap_or_default();
        let now = (js_sys::Date::now() / 1000.0) as i64;
        let back = match q["range"].as_str().unwrap_or("1h") {
            "10m" => 600,
            "6h" => 6 * 3600,
            "24h" => 86400,
            "7d" => 7 * 86400,
            _ => 3600,
        };
        let to = q["to"].as_i64().unwrap_or(now);
        let from = q["from"].as_i64().unwrap_or(to - back);
        let patterns: Vec<String> = q["series"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(String::from)).collect();
        let series = self.history.query(&patterns, from, to, q["points"].as_u64().unwrap_or(0) as usize);
        serde_json::json!({ "type": "statsResult", "id": q["id"], "from": from, "to": to, "loading": false, "series": series }).to_string()
    }
    /// A `radioQuery` (JSON) → its `radioResult` (JSON).
    pub fn radio_query(&self, query_json: &str) -> String {
        let q: serde_json::Value = serde_json::from_str(query_json).unwrap_or_default();
        self.s.radio_query(&q, (js_sys::Date::now() / 1000.0) as i64).to_string()
    }
    /// A system's talkgroup file changed (an Ignore flag): calls from now on go by it.
    pub fn set_talkgroups(&mut self, short_name: &str, csv: &str) {
        self.s.set_talkgroups(short_name, csv);
    }
    /// Preload the radio registry: JSON `{shortName: registry}` ([`WebSession::registry_unsaved`]).
    pub fn load_registry(&mut self, json: &str) {
        let m: serde_json::Map<String, serde_json::Value> = serde_json::from_str(json).unwrap_or_default();
        let shared = self.s.shared();
        let mut r = shared.radio.lock().unwrap();
        for (name, v) in m {
            if let Some(text) = v.as_str() {
                r.load(&name, text);
            }
        }
    }
    /// The registry's systems that changed since the last call, JSON `{shortName: registry}`.
    pub fn registry_unsaved(&mut self) -> String {
        let changed = self.s.shared().radio.lock().unwrap().take_dirty();
        serde_json::Value::Object(changed.into_iter().map(|(n, j)| (n, serde_json::Value::String(j))).collect()).to_string()
    }
    /// `at_ms`: when the samples arrived, on `poll`'s clock.
    pub fn push(&mut self, source: usize, bytes: &[u8], dropped: f64, at_ms: f64) {
        self.s.push(source, bytes, dropped as u64, at_ms);
    }
    pub fn add_busy_ms(&mut self, ms: f64) {
        self.s.add_busy_ms(ms);
    }
    pub fn source_error(&mut self, source: usize, error: &str) {
        self.s.source_error(source, error);
    }
    /// True when every source has ended.
    pub fn source_ended(&mut self, source: usize) -> bool {
        self.s.source_ended(source)
    }
    pub fn set_want_audio(&mut self, on: bool) {
        self.s.want_audio = on;
    }
    /// Outputs since the last poll: [{t:"text",json}|{t:"audio",system,tg,frame}|{t:"file",rel,wav,json,entry}].
    pub fn poll(&mut self, now_ms: f64) -> Array {
        self.s.poll(now_ms, &mut self.out);
        to_js(&mut self.out, &mut self.history)
    }
    pub fn finish(&mut self) -> Array {
        self.s.finish(&mut self.out);
        to_js(&mut self.out, &mut self.history)
    }
    /// Preload talker aliases: JSON `{shortName: csv}` saved from a previous
    /// run ([`WebSession::units_changed`]).
    pub fn load_units(&mut self, units_json: &str) {
        let units: serde_json::Map<String, serde_json::Value> = serde_json::from_str(units_json).unwrap_or_default();
        self.s.load_units(&|name| units.get(name).and_then(|v| v.as_str()).map(str::to_string));
    }
    /// Talker aliases of the systems that learned any since the last call, as JSON `{shortName: csv}`.
    pub fn units_changed(&mut self) -> String {
        serde_json::Value::Object(self.s.units_changed().into_iter().map(|(n, c)| (n, serde_json::Value::String(c))).collect()).to_string()
    }
    /// Preload the codes conventional frequencies carried (what
    /// [`WebSession::heard_unsaved`] gave on an earlier run).
    pub fn load_heard(&mut self, json: &str) {
        self.s.load_heard(json);
    }
    /// Those codes as JSON to save, when they changed since the last call.
    pub fn heard_unsaved(&mut self) -> Option<String> {
        self.s.heard_unsaved()
    }
    /// Each system's band plan, as JSON `{shortName: plan}`.
    pub fn bandplans(&self) -> String {
        serde_json::Value::Object(self.s.bandplans().into_iter().map(|(n, p)| (n, serde_json::Value::String(p))).collect()).to_string()
    }
}

/// The bands the survey can scan: JSON [{id, label, loHz, hiHz, defaultOn}].
#[wasm_bindgen]
pub fn survey_bands() -> String {
    trunk_app::survey::bands_json().to_string()
}

#[wasm_bindgen]
pub struct WebSurvey {
    s: SurveySession,
    out: Vec<Output>,
}

#[wasm_bindgen]
impl WebSurvey {
    /// `request_json`: {source, bands, findGain}.
    #[wasm_bindgen(constructor)]
    pub fn new(config_json: &str, request_json: &str) -> Result<WebSurvey, JsError> {
        let cfg: Config = serde_json::from_str(config_json).map_err(|e| JsError::new(&format!("config: {e}")))?;
        let req: serde_json::Value = serde_json::from_str(request_json).map_err(|e| JsError::new(&e.to_string()))?;
        let s = SurveySession::new(&cfg, &Request::from_json(&req)).map_err(|e| JsError::new(&e))?;
        Ok(WebSurvey { s, out: Vec::new() })
    }
    /// What the radio should do next: {tune: Hz} | {gain: dB} | undefined.
    pub fn command(&mut self) -> JsValue {
        match self.s.command() {
            None => JsValue::UNDEFINED,
            Some(c) => {
                let o = Object::new();
                match c {
                    Command::Tune(hz) => set(&o, "tune", hz),
                    Command::Gain(db) => set(&o, "gain", db),
                }
                o.into()
            }
        }
    }
    /// The radio applied the last command and is centred on `center_hz`.
    pub fn tuned(&mut self, center_hz: f64) {
        self.s.survey().tuned(center_hz);
    }
    pub fn push(&mut self, bytes: &[u8]) {
        self.s.survey().push_u8(bytes);
    }
    pub fn listen(&mut self, freq_hz: f64) {
        self.s.survey().listen(freq_hz);
    }
    pub fn rescan(&mut self) {
        self.s.survey().rescan();
    }
    /// The input ended (a capture file).
    pub fn finish(&mut self) {
        self.s.survey().finish();
    }
    pub fn source_error(&mut self, error: &str) {
        self.s.source_error(error);
    }
    /// Messages since the last poll: [{t:"text", json}].
    pub fn poll(&mut self, now_ms: f64) -> Array {
        self.s.poll(now_ms, &mut self.out);
        to_js(&mut self.out, &mut History::new(2))
    }
}

/// How a source's band rolls off at its edges (`profileSource`).
#[wasm_bindgen]
pub struct WebProfiler {
    p: trunk_core::dsp::rolloff::Profiler,
    source: usize,
    center_hz: f64,
}

#[wasm_bindgen]
impl WebProfiler {
    #[wasm_bindgen(constructor)]
    pub fn new(source: usize, center_hz: f64, rate_hz: f64) -> WebProfiler {
        WebProfiler { p: trunk_core::dsp::rolloff::Profiler::new(rate_hz, trunk_app::profile::SECONDS), source, center_hz }
    }
    pub fn push(&mut self, bytes: &[u8]) {
        self.p.push_u8(bytes);
    }
    pub fn done(&self) -> bool {
        self.p.done()
    }
    /// The `sourceProfile` message (JSON), or its error if nothing was seen.
    pub fn result(&self) -> String {
        match self.p.result() {
            Some(r) => trunk_app::profile::json(self.source, self.center_hz, &r),
            None => trunk_app::profile::error_json(self.source, "The source ended before anything could be seen."),
        }
        .to_string()
    }
}

/// The driver keeps this many USB transfers in flight.
const IN_FLIGHT: usize = 8;

struct Rtl {
    dev: Device<RawIq>,
    rx: RxStream<RawIq>,
    /// Blocks still to drop after a retune: queued before it.
    stale: usize,
}

/// An RTL-SDR over WebUSB. Methods return promises; call them one at a time.
#[wasm_bindgen]
pub struct WebRtl {
    inner: Rc<RefCell<Option<Rtl>>>,
    /// Set by `close`: a pending `next` hands the device back at its next block.
    closing: Rc<Cell<bool>>,
}

fn err(e: impl std::fmt::Display) -> JsValue {
    JsError::new(&e.to_string()).into()
}

#[wasm_bindgen]
impl WebRtl {
    /// Dongles this page has been granted: [{index, serial, product}].
    pub async fn devices() -> Result<Array, JsValue> {
        let list = Device::list().await.map_err(err)?;
        let a = Array::new();
        for d in list {
            let o = Object::new();
            set(&o, "index", d.index as u32);
            set(&o, "serial", d.serial.clone().unwrap_or_default());
            set(&o, "product", d.product.clone().unwrap_or_else(|| "RTL-SDR".into()));
            a.push(&o);
        }
        Ok(a)
    }

    /// Open (serial "" = first granted dongle), tune and start streaming.
    pub async fn open(serial: String, center_hz: f64, rate_hz: u32, gain_db: Option<f32>, ppm: i32) -> Result<WebRtl, JsValue> {
        let mut b = Device::builder().raw_iq().frequency_hz(center_hz as u64).sample_rate_hz(rate_hz).correction_ppm(ppm);
        b = b.gain(gain_db.map_or(GainConfig::Auto, GainConfig::Manual));
        if !serial.is_empty() {
            b = b.serial(serial);
        }
        let dev = b.open().await.map_err(|e| match e {
            rtlsdr_nusb::Error::UnsupportedTuner => err("this dongle's tuner isn't supported in the browser (only R820T / R828D; not E4000, FC0012, FC0013 or FC2580) — the desktop app can use it as a SoapySDR source with driver=rtlsdr"),
            e => err(e),
        })?;
        let mut rx = dev.rx_stream().map_err(err)?;
        rx.start().await.map_err(err)?;
        Ok(WebRtl { inner: Rc::new(RefCell::new(Some(Rtl { dev, rx, stale: 0 }))), closing: Rc::new(Cell::new(false)) })
    }

    /// The next block: {bytes: Uint8Array, dropped} (undefined when closed).
    pub fn next(&self) -> js_sys::Promise {
        let inner = self.inner.clone();
        let closing = self.closing.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let mut g = inner.borrow_mut();
            let Some(r) = g.as_mut() else { return Ok(JsValue::UNDEFINED) };
            loop {
                let Some(b) = r.rx.next_block(None).await.map_err(err)? else { return Ok(JsValue::UNDEFINED) };
                if closing.get() {
                    return Ok(JsValue::UNDEFINED);
                }
                if r.stale > 0 {
                    r.stale -= 1;
                    continue;
                }
                let o = Object::new();
                set(&o, "bytes", Uint8Array::from(b.raw_bytes()));
                set(&o, "dropped", b.dropped_samples() as f64);
                return Ok(o.into());
            }
        })
    }

    /// Retune while streaming, dropping the blocks already queued (from the
    /// old frequency); resolves to the centre actually tuned. Not stop/start:
    /// WebUSB can't cancel the queued transfers, and after the device's FIFO
    /// reset they never complete, so the stream would stall.
    pub fn retune(&self, center_hz: f64) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let mut g = inner.borrow_mut();
            let Some(r) = g.as_mut() else { return Err(err("closed")) };
            r.dev.set_frequency_hz(center_hz.round() as u64).await.map_err(err)?;
            r.stale = IN_FLIGHT;
            Ok(JsValue::from_f64(r.dev.actual_frequency_hz() as f64))
        })
    }

    /// Set a manual gain (dB); resolves to the tuned centre.
    pub fn set_gain(&self, gain_db: f32) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let mut g = inner.borrow_mut();
            let Some(r) = g.as_mut() else { return Err(err("closed")) };
            r.dev.set_gain(GainConfig::Manual(gain_db)).await.map_err(err)?;
            Ok(JsValue::from_f64(r.dev.actual_frequency_hz() as f64))
        })
    }

    /// Close; a `next` (or retune) still pending holds the device until it
    /// resolves (WebUSB can't cancel it), so wait for that, up to 2 s.
    pub fn close(&self) -> js_sys::Promise {
        let inner = self.inner.clone();
        self.closing.set(true);
        wasm_bindgen_futures::future_to_promise(async move {
            let mut taken = None;
            for _ in 0..200 {
                if let Ok(mut g) = inner.try_borrow_mut() {
                    taken = g.take();
                    break;
                }
                sleep_ms(10).await;
            }
            if let Some(Rtl { mut dev, rx, .. }) = taken {
                let _ = rx.close().await;
                let _ = dev.shutdown().await;
            }
            Ok(JsValue::UNDEFINED)
        })
    }
}
