//! Trunk Recorder Lite in the browser. Exposes to the engine worker
//! (web/src/web/engine.worker.ts):
//!
//! * [`WebSession`] — the shared recording session (trunk-app): feed it u8
//!   IQ, poll it for interface messages, live audio and call files.
//! * [`WebRtl`] — an RTL-SDR dongle over WebUSB (rtlsdr-nusb on nusb's WebUSB
//!   backend; works in a worker once the page has been granted the device).
//!
//! Build: `cargo build -p trunk-web --target wasm32-unknown-unknown --profile
//! dist` with `-C target-feature=+simd128`, then `wasm-bindgen --target web`
//! (see web/package.json `build:wasm`).

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Object, Reflect, Uint8Array};
use rtlsdr_nusb::{Device, GainConfig, RawIq, RxStream};
use trunk_app::{Config, Output, Session};
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
            Output::Text(t) => {
                set(&obj, "t", "text");
                set(&obj, "json", t);
            }
            Output::Audio { tg, frame } => {
                set(&obj, "t", "audio");
                set(&obj, "tg", tg);
                set(&obj, "frame", Uint8Array::from(frame.as_slice()));
            }
            Output::File { rel, wav, json, entry } => {
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
    /// `config_json`: the interface's Config. `bandplan`: saved from a previous run.
    #[wasm_bindgen(constructor)]
    pub fn new(config_json: &str, epoch_ms: f64, bandplan: Option<String>) -> Result<WebSession, JsError> {
        let cfg: Config = serde_json::from_str(config_json).map_err(|e| JsError::new(&format!("config: {e}")))?;
        let s = Session::new(cfg, epoch_ms, bandplan.as_deref(), local_ymd).map_err(|e| JsError::new(&e))?;
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
    /// Outputs since the last poll: [{t:"text",json}|{t:"audio",tg,frame}|{t:"file",rel,wav,json,entry}].
    pub fn poll(&mut self, now_ms: f64) -> Array {
        self.s.poll(now_ms, &mut self.out);
        to_js(&mut self.out)
    }
    pub fn finish(&mut self) -> Array {
        self.s.finish(&mut self.out);
        to_js(&mut self.out)
    }
    pub fn bandplan(&self) -> String {
        self.s.bandplan()
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
