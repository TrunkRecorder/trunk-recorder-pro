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
//! And to the page (both builds'): the config's rules ([`rules`]).
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

pub mod rules;

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
            // The worker puts it in the config ([`learn_call_sign`]).
            Output::CallSign { short_name, freq_hz, call_sign } => {
                set(&obj, "t", "callSign");
                set(&obj, "shortName", short_name);
                set(&obj, "freqHz", freq_hz);
                set(&obj, "callSign", call_sign);
            }
            // The worker sends it to the directory, then says how it went ([`WebSession::share_done`]).
            Output::Share { short_name, observation, site_names } => {
                set(&obj, "t", "share");
                set(&obj, "shortName", short_name);
                set(&obj, "json", observation.to_string());
                set(&obj, "siteNames", serde_json::to_string(&site_names).unwrap_or_default());
            }
            // (One page, which asked for its topics.)
            Output::Message(m) | Output::Topic { msg: m, .. } => {
                set(&obj, "t", "text");
                set(&obj, "json", m.to_json());
            }
            Output::Audio { system, short_name, tg, frame, .. } => {
                set(&obj, "t", "audio");
                set(&obj, "system", system);
                set(&obj, "shortName", short_name);
                set(&obj, "tg", tg);
                set(&obj, "frame", Uint8Array::from(frame.as_slice()));
            }
            // (No frame capture in the browser: the setting is desktop-only.)
            Output::File { rel, wav, record, .. } => {
                set(&obj, "t", "file");
                set(&obj, "wav", Uint8Array::from(wav.as_slice()));
                set(&obj, "json", record.to_json());
                set(&obj, "entry", trunk_app::session::call_entry(&rel, &record).to_string());
                set(&obj, "rel", rel);
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
    /// `config_json`: the interface's Config. `talkgroups_json`: each system's talkgroup file, `{shortName: csv}`.
    /// `bandplans_json`: `{key: plan}` (a site's key: [`trunk_app::session::bandplan_key`]) saved from a previous run
    /// ([`WebSession::due`]).
    #[wasm_bindgen(constructor)]
    pub fn new(config_json: &str, talkgroups_json: &str, epoch_ms: f64, bandplans_json: Option<String>) -> Result<WebSession, JsError> {
        let cfg = Config::from_json(config_json).map_err(|e| JsError::new(&format!("config: {e}")))?;
        let talkgroups: trunk_app::config::TalkgroupFiles = serde_json::from_str(talkgroups_json).unwrap_or_default();
        let plans: serde_json::Map<String, serde_json::Value> = bandplans_json.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
        let plan = |name: &str| plans.get(name).and_then(|v| v.as_str()).map(str::to_string);
        let s = Session::new(cfg, &talkgroups, epoch_ms, &plan, local_offset).map_err(|e| JsError::new(&e))?;
        Ok(WebSession { s, out: Vec::new(), history: History::default() })
    }
    /// What the page watches: JSON `["spectrum:0", "log", …]`.
    pub fn set_topics(&mut self, topics_json: &str) {
        let t: Vec<String> = serde_json::from_str(topics_json).unwrap_or_default();
        self.s.set_topics(Topics::new(t));
    }
    /// A `statsQuery` (JSON) → its `statsResult` (JSON), from this session's minutes.
    pub fn stats_query(&self, query_json: &str) -> String {
        let q: trunk_app::protocol::StatsQuery = serde_json::from_str(query_json).unwrap_or_default();
        q.answer(&self.history, (js_sys::Date::now() / 1000.0) as i64, false).to_json()
    }
    /// A `radioQuery` (JSON) → its `radioResult` (JSON).
    pub fn radio_query(&self, query_json: &str) -> String {
        let q: serde_json::Value = serde_json::from_str(query_json).unwrap_or_default();
        self.s.shared().radio_query(&q, (js_sys::Date::now() / 1000.0) as i64).to_json()
    }
    /// A system's talkgroup file changed (an Ignore flag): calls from now on go by it.
    pub fn set_talkgroups(&mut self, short_name: &str, csv: &str) {
        self.s.set_talkgroups(short_name, csv);
    }
    /// This browser's directory recorder ID ([`directory_recorder_id`]): shares go under it.
    pub fn set_recorder(&mut self, id: &str) {
        self.s.set_recorder(id);
    }
    /// A system's sharing switched, or its directory link changed: `system_json`, its `System`.
    pub fn set_share(&mut self, system_json: &str) {
        if let Ok(sys) = serde_json::from_str::<trunk_app::config::System>(system_json) {
            self.s.set_share(&sys);
        }
    }
    /// How system `short_name`'s share went: `error` when it didn't.
    /// `id` and `rev`: the directory's record and version for it (none: taken, not linked); `error`: why not.
    pub fn share_done(&mut self, short_name: &str, now_ms: f64, id: Option<String>, rev: f64, error: Option<String>) {
        let linked = id.map(|id| trunk_app::share::Linked { id, rev: rev.max(0.0) as u64 });
        self.s.share_done(short_name, now_ms, error.map_or(Ok(linked), Err));
    }
    /// What sharing did before ([`WebSession::due`] gave): each directory record's version and last share.
    pub fn load_sharing(&mut self, json: &str) {
        self.s.load_sharing(json);
    }
    /// Preload the radio registry: JSON `{shortName: registry}` (each as [`WebSession::due`] gave it).
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
    /// run ([`WebSession::due`]).
    pub fn load_units(&mut self, units_json: &str) {
        let units: serde_json::Map<String, serde_json::Value> = serde_json::from_str(units_json).unwrap_or_default();
        self.s.load_units(&|name| units.get(name).and_then(|v| v.as_str()).map(str::to_string));
    }
    /// Preload the codes conventional frequencies carried (what
    /// [`WebSession::due`] gave on an earlier run).
    pub fn load_heard(&mut self, json: &str) {
        self.s.load_heard(json);
    }
    /// Start AutoTune sources from the scores [`WebSession::due`] gave on an earlier run.
    pub fn load_tune(&mut self, json: &str) {
        self.s.load_tune(json);
    }
    /// What's due to be kept between runs ([`trunk_app::saving`] says when),
    /// as JSON `[{kind, name, text}]`: `kind` units (`name`: the system),
    /// sharing, bandplan (`name`: the site's key), heard, tune or registry
    /// (`name`: the system). `all`: everything that changed (the recording's end).
    pub fn due(&mut self, now_ms: f64, all: bool) -> String {
        use trunk_app::saving::Saved;
        let item = |kind: &str, name: &str, text: String| serde_json::json!({ "kind": kind, "name": name, "text": text });
        let items: Vec<serde_json::Value> = self
            .s
            .due(now_ms, all)
            .into_iter()
            .filter_map(|saved| {
                Some(match saved {
                    Saved::Units { system, csv } => item("units", &system, csv),
                    Saved::Sharing(json) => item("sharing", "", json),
                    Saved::Bandplan { key, plan } => item("bandplan", &key, plan),
                    Saved::Heard(json) => item("heard", "", json),
                    Saved::Tune(json) => item("tune", "", json),
                    Saved::Registry { system, radio } => item("registry", &system, serde_json::to_string(&radio).ok()?),
                })
            })
            .collect();
        serde_json::Value::Array(items).to_string()
    }
    /// Keep the radio registry every `ms` (a tab can be closed without the recording's end).
    pub fn save_registry_every(&mut self, ms: f64) {
        self.s.save_registry_every(ms);
    }
}

/// A call sign heard on the air for `freq_hz` of system `short_name`, into
/// config `config_json`'s FCC licenses ([`Config::learn_call_sign`]): the
/// config (JSON) when it changed, else undefined.
#[wasm_bindgen]
pub fn learn_call_sign(config_json: &str, short_name: &str, freq_hz: f64, call_sign: &str) -> Option<String> {
    let mut cfg = Config::from_json(config_json).ok()?;
    cfg.learn_call_sign(short_name, freq_hz, call_sign).then(|| serde_json::to_string(&cfg).unwrap_or_default())
}

/// config `config_json` with system `short_name` linked to the directory's
/// records, as a share's answer gave them ([`Config::link_shared`]): the
/// config (JSON) when it changed (a first share, a new site), else undefined. `site_names_json`, `site_ids_json`: arrays.
#[wasm_bindgen]
pub fn link_shared(config_json: &str, short_name: &str, id: &str, site_names_json: &str, site_ids_json: &str) -> Option<String> {
    let mut cfg = Config::from_json(config_json).ok()?;
    let names: Vec<String> = serde_json::from_str(site_names_json).ok()?;
    let ids: Vec<String> = serde_json::from_str(site_ids_json).ok()?;
    cfg.link_shared(short_name, id, &names, &ids).then(|| serde_json::to_string(&cfg).unwrap_or_default())
}

/// A new directory recorder ID from 16 random bytes.
#[wasm_bindgen]
pub fn directory_recorder_id(random: &[u8]) -> String {
    let mut b = [0u8; 16];
    for (d, s) in b.iter_mut().zip(random) {
        *d = *s;
    }
    trunk_app::share::recorder_id(&b)
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
        let cfg = Config::from_json(config_json).map_err(|e| JsError::new(&format!("config: {e}")))?;
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
