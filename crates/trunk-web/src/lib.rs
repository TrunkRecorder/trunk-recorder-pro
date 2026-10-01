//! Trunk Recorder Pro in the browser. Exposes to the engine worker
//! (web/src/web/engine.worker.ts):
//!
//! * [`WebSession`] — the shared recording session (trunk-app): feed it u8
//!   IQ, poll it for interface messages, live audio and call files.
//! * [`WebRtl`] — an RTL-SDR dongle over WebUSB (rtlsdr-nusb on nusb's WebUSB
//!   backend; works in a worker once the page has been granted the device).
//! * [`WebSurvey`] — the first-run survey (trunk-app): it asks for retunes,
//!   takes u8 IQ, and reports `survey` / `surveySpectrum` messages.
//!
//! Build: `cargo build -p trunk-web --target wasm32-unknown-unknown --profile
//! dist` with `-C target-feature=+simd128`, then `wasm-bindgen --target web`
//! (see web/package.json `build:wasm`).

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Object, Reflect, Uint8Array};
use rtlsdr_nusb::{Device, GainConfig, RawIq, RxStream};
use trunk_app::survey::{Request, SurveySession};
use trunk_app::{Config, Output, Session};
use trunk_core::survey::Command;
use wasm_bindgen::prelude::*;

fn local_ymd(t: i64) -> (i32, u32, u32) {
    let d = js_sys::Date::new(&JsValue::from_f64(t as f64 * 1000.0));
    (d.get_full_year() as i32, d.get_month() + 1, d.get_date())
}

fn set(o: &Object, k: &str, v: impl Into<JsValue>) {
    let _ = Reflect::set(o, &JsValue::from_str(k), &v.into());
}

fn to_js(out: &mut Vec<Output>) -> Array {
    let a = Array::new();
    for o in out.drain(..) {
        let obj = Object::new();
        match o {
            // (No plugins in the browser; none are asked for.)
            Output::Plugin(_) => continue,
            Output::Text(t) => {
                set(&obj, "t", "text");
                set(&obj, "json", t);
            }
            Output::Audio { system, tg, frame } => {
                set(&obj, "t", "audio");
                set(&obj, "system", system);
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
        let s = Session::new(cfg, epoch_ms, &plan, local_ymd).map_err(|e| JsError::new(&e))?;
        Ok(WebSession { s, out: Vec::new() })
    }
    pub fn push(&mut self, source: usize, bytes: &[u8], dropped: f64) {
        self.s.push(source, bytes, dropped as u64);
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
        to_js(&mut self.out)
    }
    pub fn finish(&mut self) -> Array {
        self.s.finish(&mut self.out);
        to_js(&mut self.out)
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
        to_js(&mut self.out)
    }
}

struct Rtl {
    dev: Device<RawIq>,
    rx: RxStream<RawIq>,
}

/// An RTL-SDR over WebUSB. Methods return promises; call them one at a time.
#[wasm_bindgen]
pub struct WebRtl {
    inner: Rc<RefCell<Option<Rtl>>>,
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
        let dev = b.open().await.map_err(err)?;
        let mut rx = dev.rx_stream().map_err(err)?;
        rx.start().await.map_err(err)?;
        Ok(WebRtl { inner: Rc::new(RefCell::new(Some(Rtl { dev, rx }))) })
    }

    /// The next block: {bytes: Uint8Array, dropped} (undefined when closed).
    pub fn next(&self) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let mut g = inner.borrow_mut();
            let Some(r) = g.as_mut() else { return Ok(JsValue::UNDEFINED) };
            match r.rx.next_block(None).await.map_err(err)? {
                Some(b) => {
                    let o = Object::new();
                    set(&o, "bytes", Uint8Array::from(b.raw_bytes()));
                    set(&o, "dropped", b.dropped_samples() as f64);
                    Ok(o.into())
                }
                None => Ok(JsValue::UNDEFINED),
            }
        })
    }

    /// Retune (the stream is stopped meanwhile, so no queued block is from
    /// the old frequency); resolves to the centre actually tuned.
    pub fn retune(&self, center_hz: f64) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let mut g = inner.borrow_mut();
            let Some(r) = g.as_mut() else { return Err(err("closed")) };
            r.rx.stop().await.map_err(err)?;
            r.dev.set_frequency_hz(center_hz.round() as u64).await.map_err(err)?;
            r.rx.start().await.map_err(err)?;
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

    pub fn close(&self) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let taken = inner.borrow_mut().take();
            if let Some(Rtl { mut dev, rx }) = taken {
                let _ = rx.close().await;
                let _ = dev.shutdown().await;
            }
            Ok(JsValue::UNDEFINED)
        })
    }
}
