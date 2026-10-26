//! The first-run survey for the interface, shared by the desktop app and the
//! web build: the core [`Survey`] set up from a configured source, reported
//! as `survey` messages (a whole snapshot, ~2.5/s) and `surveySpectrum`, with
//! a suggested setup once the control channel has told enough — the control
//! channels, the ppm to set, the gain, and a centre that covers the most
//! voice channels seen.
//!
//! Looking for a system ([`LookFor`]: a directory system known by its
//! licenses or its sites, a sites file, a system set up already) it scans
//! just their frequencies, and judges each of its sites by what they showed
//! ([`trunk_core::sitecheck`]): which can be heard here. On a spare radio
//! (one that isn't a source) it runs beside the recording.
//!
//! The platform drives it: [`SurveySession::survey`]'s commands tune the
//! radio; samples and tune confirmations go back in.

use serde_json::{json, Value};
use trunk_core::sitecheck::{self, Site};
use trunk_core::survey::{Command, GainState, Kind, Receivers, Stage, Survey, SurveyConfig, BANDS};

use crate::config::{auto_center, usable_half_width, Config, SiteIdentity, Source};
use crate::protocol::ToClient;
#[cfg(test)]
use crate::config::DEFAULT_GUARD_HZ;
use crate::session::Output;

/// R820T / R828D gain steps worth trying (dB).
const RTL_GAINS: [f32; 7] = [19.7, 28.0, 33.8, 38.6, 42.1, 44.5, 49.6];

/// The bands the survey knows, for the interface.
pub fn bands_json() -> Value {
    BANDS.iter().map(|b| json!({ "id": b.id, "label": b.label, "loHz": b.lo_hz, "hiHz": b.hi_hz, "defaultOn": b.default_on })).collect()
}

/// What `surveyStart` asks for.
#[derive(Clone, Debug)]
pub struct Request {
    pub source: usize,
    /// A radio that isn't one of the sources (a spare: it may scan while the
    /// others record), in place of `source`.
    pub radio: Option<Source>,
    pub bands: Vec<String>,
    /// Step the gain while monitoring (RTL-SDR).
    pub find_gain: bool,
    pub look_for: Option<LookFor>,
    /// CPU cores the probes may use (the platform's to say).
    pub threads: usize,
}

/// A system the survey looks for: the frequencies to scan (its licensed
/// ones, its sites'), and its sites, each judged by what its showed.
#[derive(Clone, Debug, Default)]
pub struct LookFor {
    pub freqs_hz: Vec<f64>,
    pub sites: Vec<Site>,
    /// "p25", "smartnet", "edacs", "dmr", "nxdn"; None: not known (every receiver runs).
    pub system_type: Option<String>,
    /// How long each frequency is listened to, s.
    pub listen_s: f64,
}

/// Each frequency's listen when none is asked for, s: long enough for a
/// control channel to say who it is; a DMR site without one (Capacity Plus)
/// may only key up for a call.
fn default_listen_s(system_type: Option<&str>) -> f64 {
    match system_type {
        Some("dmr") => 60.0,
        Some(_) => 5.0,
        None => 2.0,
    }
}

impl LookFor {
    /// `lookFor`: { freqsHz, sites: [{ label, freqsHz, expect, colorCode }], type, listenS }.
    pub fn from_json(v: &Value) -> Option<LookFor> {
        let hz = |v: &Value| -> Vec<f64> { v.as_array().map(|a| a.iter().filter_map(Value::as_f64).filter(|&f| f > 0.0).collect()).unwrap_or_default() };
        let sites: Vec<Site> = v["sites"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|s| Site {
                        label: s["label"].as_str().unwrap_or("").to_string(),
                        freqs_hz: hz(&s["freqsHz"]),
                        expect: serde_json::from_value::<SiteIdentity>(s["expect"].clone()).unwrap_or_default().engine(),
                        color_code: s["colorCode"].as_u64().map(|c| c as u8),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut freqs_hz = hz(&v["freqsHz"]);
        freqs_hz.extend(sites.iter().flat_map(|s| s.freqs_hz.iter().copied()));
        freqs_hz.sort_by(f64::total_cmp);
        freqs_hz.dedup();
        if freqs_hz.is_empty() {
            return None;
        }
        let system_type = v["type"].as_str().filter(|t| ["p25", "smartnet", "edacs", "dmr", "nxdn"].contains(t)).map(str::to_string);
        let listen_s = v["listenS"].as_f64().filter(|&s| s > 0.0).unwrap_or_else(|| default_listen_s(system_type.as_deref())).clamp(0.5, 600.0);
        Some(LookFor { freqs_hz, sites, system_type, listen_s })
    }
}

impl Request {
    pub fn from_json(v: &Value) -> Request {
        let bands = v["bands"].as_array().map(|a| a.iter().filter_map(|b| b.as_str().map(str::to_string)).collect::<Vec<_>>());
        Request {
            source: v["source"].as_u64().unwrap_or(0) as usize,
            radio: v.get("radio").filter(|r| r.is_object()).and_then(|r| serde_json::from_value(r.clone()).ok()),
            bands: bands.filter(|b| !b.is_empty()).unwrap_or_else(|| BANDS.iter().filter(|b| b.default_on).map(|b| b.id.to_string()).collect()),
            find_gain: v["findGain"].as_bool().unwrap_or(true),
            look_for: LookFor::from_json(&v["lookFor"]),
            threads: 1,
        }
    }

    /// The radio it scans with: the spare, else source `source`.
    pub fn radio<'a>(&'a self, cfg: &'a Config) -> Result<&'a Source, String> {
        match &self.radio {
            Some(r) => Ok(r),
            None => cfg.sources.get(self.source).ok_or_else(|| "No such source.".into()),
        }
    }
}

/// The survey settings for the request's radio.
pub fn survey_config(cfg: &Config, req: &Request) -> Result<SurveyConfig, String> {
    let src = req.radio(cfg)?;
    let mut sc = SurveyConfig { rate_hz: src.rate_hz(), bands: req.bands.clone(), threads: req.threads.max(1), ..Default::default() };
    if let Some(l) = &req.look_for {
        sc.targets = l.freqs_hz.clone();
        sc.probe_s = l.listen_s;
        sc.receivers = l.system_type.as_deref().map_or(Receivers::ALL, Receivers::of_type);
    }
    match src {
        Source::Rtlsdr { ppm, .. } => {
            sc.ppm = *ppm as f64;
            sc.tune_range_hz = (24e6, 1766e6);
            sc.settle_s = 0.03;
            if req.find_gain {
                sc.gains = RTL_GAINS.to_vec();
            }
        }
        Source::Usrp { ppm, .. } => {
            sc.ppm = *ppm;
            sc.tune_range_hz = (10e6, 6e9);
            sc.settle_s = 0.15;
        }
        Source::Airspy { ppm, .. } => {
            sc.ppm = *ppm;
            sc.tune_range_hz = (24e6, 1800e6);
            sc.settle_s = 0.2;
        }
        // bladeRF 2.0 (AD9361): 47 MHz – 6 GHz.
        Source::Bladerf { ppm, .. } => {
            sc.ppm = *ppm;
            sc.tune_range_hz = (47e6, 6e9);
            sc.settle_s = 0.15;
        }
        // The device's range isn't known here: the widest any module offers; a
        // tune it refuses shows as a source error.
        Source::Soapy { ppm, .. } => {
            sc.ppm = *ppm;
            sc.tune_range_hz = (1e6, 6e9);
            sc.settle_s = 0.2;
        }
        Source::File { center_hz, .. } => {
            let c = if req.radio.is_some() { *center_hz } else { cfg.resolved_centers().get(req.source).copied().unwrap_or(0.0) };
            if c <= 0.0 {
                return Err("Set the capture's center frequency first.".into());
            }
            sc.fixed_center_hz = Some(c);
        }
    }
    if sc.rate_hz < 240_000.0 {
        return Err("The source's sample rate is too low to scan.".into());
    }
    // Finding systems not set up yet: every receiver (C4FM and CQPSK).
    sc.bank.cqpsk = true;
    sc.bank.cqpsk_eq = true;
    sc.bank.c4fm = true;
    Ok(sc)
}

pub struct SurveySession {
    survey: Survey,
    source: usize,
    /// The spare radio scanned with, for the interface ("RTL-SDR SN 200"); None: source `source`.
    spare: Option<String>,
    look_for: Option<LookFor>,
    rtl: bool,
    /// The source's guard band at each edge, Hz.
    guard_hz: f64,
    bands: Vec<String>,
    error: Option<String>,
    last_ms: f64,
    last_spec_ms: f64,
    changed: bool,
}

impl SurveySession {
    pub fn new(cfg: &Config, req: &Request) -> Result<SurveySession, String> {
        let sc = survey_config(cfg, req)?;
        let src = req.radio(cfg)?;
        Ok(SurveySession {
            survey: Survey::new(sc),
            source: req.source,
            spare: req.radio.as_ref().map(Source::label),
            look_for: req.look_for.clone(),
            rtl: matches!(src, Source::Rtlsdr { .. }),
            guard_hz: src.guard_hz(),
            bands: req.bands.clone(),
            error: None,
            last_ms: f64::NEG_INFINITY,
            last_spec_ms: f64::NEG_INFINITY,
            changed: true,
        })
    }

    /// The survey itself: feed it, and ask it what the radio should do.
    pub fn survey(&mut self) -> &mut Survey {
        self.changed = true;
        &mut self.survey
    }

    /// Take the survey's next radio command.
    pub fn command(&mut self) -> Option<Command> {
        self.survey.command()
    }

    pub fn source_error(&mut self, e: &str) {
        self.error = Some(e.to_string());
        self.changed = true;
    }

    /// What to look licenses up for: the monitored system's control
    /// channels first (the ones that name it), then the signals found, the
    /// strongest first (not the weak unknown ones).
    pub fn heard(&self) -> Vec<crate::licenses::Signal> {
        let s = &self.survey;
        let mut out: Vec<crate::licenses::Signal> = Vec::new();
        if let Some(m) = s.system() {
            out.extend(m.advertised_hz.into_iter().chain(m.secondary.iter().copied()).map(|f| crate::licenses::Signal { freq_hz: f as f64, width_hz: None }));
        }
        let mut cands: Vec<_> = s.candidates().iter().filter(|c| c.kind != Kind::Other || c.snr_db >= 10.0).collect();
        cands.sort_by(|a, b| (a.kind == Kind::Other).cmp(&(b.kind == Kind::Other)).then(b.snr_db.total_cmp(&a.snr_db)));
        out.extend(cands.iter().map(|c| crate::licenses::Signal { freq_hz: s.corrected(c.freq_hz).unwrap_or(c.freq_hz), width_hz: Some(c.width_hz) }));
        out
    }

    /// A snapshot every 400 ms of wall clock (at once after a stage change),
    /// the spectrum every 150 ms.
    pub fn poll(&mut self, now_ms: f64, out: &mut Vec<Output>) {
        if now_ms - self.last_spec_ms >= 150.0 {
            self.last_spec_ms = now_ms;
            if let Some((center, bins)) = self.survey.spectrum(512) {
                let bins: Vec<f32> = bins.iter().map(|v| (v * 10.0).round() / 10.0).collect();
                out.push(Output::Message(ToClient::SurveySpectrum { source: self.source, center_hz: center, rate_hz: self.survey.rate_hz(), bins }));
            }
        }
        if self.changed && now_ms - self.last_ms >= 400.0 {
            self.last_ms = now_ms;
            self.changed = false;
            out.push(Output::Message(ToClient::Survey(crate::protocol::body(self.snapshot()))));
        }
    }

    pub fn snapshot(&self) -> Value {
        let s = &self.survey;
        let id = crate::session::identity_json;
        let candidates: Vec<Value> = s
            .candidates()
            .iter()
            // (A frequency looked for is shown whatever it showed: a site check needs its quiet ones.)
            .filter(|c| c.target.is_some() || c.kind != Kind::Other || c.snr_db >= 6.0)
            .map(|c| {
                json!({
                    "freqHz": c.freq_hz.round(), "correctedHz": s.corrected(c.freq_hz).map(f64::round), "band": c.band, "snrDb": (c.snr_db * 10.0).round() / 10.0,
                    "widthHz": c.width_hz.round(), "kind": c.kind.as_str(), "frames": c.frames, "good": c.good, "bad": c.bad,
                    "modulation": c.modulation, "identity": id(&c.identity),
                    "dmr": c.dmr.map(|d| json!({ "variant": d.variant.map(|v| v.name()), "colorCode": d.color_code })),
                    "nxdn": c.nxdn.map(|n| json!({ "rate": n.rate.name(), "ran": n.ran, "system": n.location.map(|l| l.0), "site": n.location.map(|l| l.1) })),
                    "targetHz": c.target, "up": c.up.map(|u| (u * 100.0).round() / 100.0),
                })
            })
            .collect();
        let monitor = s.system().map(|m| {
            json!({
                "freqHz": s.monitoring_hz().map(f64::round), "heardHz": m.heard_hz.round(), "identity": id(&m.identity), "advertisedHz": m.advertised_hz,
                "secondary": m.secondary, "adjacent": m.adjacent.iter().map(|a| json!({ "rfss": a.rfss, "site": a.site, "sysId": a.sys_id, "freqHz": a.freq_hz })).collect::<Vec<_>>(),
                "voice": m.voice.iter().map(|v| json!({ "freqHz": v.freq_hz, "grants": v.grants, "tdma": v.tdma })).collect::<Vec<_>>(),
                "idens": m.idens, "good": m.good, "bad": m.bad, "airS": (m.air_s * 100.0).round() / 100.0, "modulation": m.modulation, "snrDb": m.snr_db.map(|v| (v * 10.0).round() / 10.0),
                "offsetHz": m.offset_hz.map(f64::round), "ppm": m.ppm.map(|p| (p * 100.0).round() / 100.0),
                "gain": {
                    "state": match m.gain.state { GainState::Off => "off", GainState::Waiting => "waiting", GainState::Running => "running", GainState::Done => "done" },
                    "steps": m.gain.steps.iter().map(|g| json!({ "gainDb": (g.gain_db as f64 * 10.0).round() / 10.0, "snrDb": (g.snr_db * 10.0).round() / 10.0, "okRatio": g.ok_ratio, "clipped": g.clipped })).collect::<Vec<_>>(),
                    "bestDb": m.gain.best_db.map(|g| (g as f64 * 10.0).round() / 10.0),
                },
                "elapsedS": m.elapsed_s, "ready": m.ready(),
                "smartnet": m.smartnet.as_ref().map(|s| json!({
                    "ccChan": s.cc_chan, "altChans": s.alt_chans, "channels": s.channels,
                    "points": s.points.iter().map(|p| json!({ "chan": p.chan, "hz": p.hz.round(), "riseDb": (p.rise_db * 10.0).round() / 10.0 })).collect::<Vec<_>>(),
                    "spacingHz": s.fit.map(|f| f.spacing_hz), "inliers": s.fit.map(|f| f.inliers),
                    "bandplan": s.plan.as_ref().map(plan_json),
                })),
            })
        });
        let progress = s.progress().map(|(hop, hops, band, center)| json!({ "hop": hop, "hops": hops, "band": band, "centerHz": center }));
        json!({
            "type": "survey",
            "stage": match s.stage() { Stage::Scanning => "scanning", Stage::Monitoring => "monitoring", Stage::Done => "done" },
            "source": if self.spare.is_some() { Value::Null } else { json!(self.source) },
            "radio": self.spare,
            "lookFor": self.look_for.as_ref().map(|l| json!({ "freqs": l.freqs_hz.len(), "probed": s.probed().len(), "listenS": l.listen_s, "type": l.system_type })),
            "sites": self.sites(),
            "bands": self.bands,
            "message": s.message(),
            "error": self.error,
            "progress": progress,
            "candidates": candidates,
            "monitor": monitor,
            "suggest": self.suggest(),
        })
    }

    /// Each site looked for, judged by what its frequencies showed so far.
    fn sites(&self) -> Vec<Value> {
        let Some(l) = &self.look_for else { return vec![] };
        let s = &self.survey;
        let ty = l.system_type.as_deref().unwrap_or("");
        let judged = sitecheck::judge_all(ty, &l.sites, s.candidates(), s.probed());
        let scanning = s.stage() == Stage::Scanning;
        l.sites
            .iter()
            .zip(judged)
            .map(|(site, j)| {
                // Not listened to: not yet, or out of the radio's range.
                let why = if j.verdict == sitecheck::Verdict::NotChecked && !scanning { "outside what this radio tunes".to_string() } else if j.verdict == sitecheck::Verdict::NotChecked { "not listened to yet".to_string() } else { j.why };
                let c = j.freq_hz.and_then(|f| s.candidates().iter().find(|c| c.target == Some(f)));
                json!({ "label": site.label, "verdict": j.verdict.as_str(), "why": why, "freqHz": j.freq_hz, "quality": j.quality.map(|q| q.as_str()), "snrDb": c.map(|c| (c.snr_db * 10.0).round() / 10.0) })
            })
            .collect()
    }

    /// The setup to apply, once the monitored channel announced itself.
    fn suggest(&self) -> Option<Value> {
        let m = self.survey.system()?;
        let cc = m.advertised_hz? as f64;
        let mut ccs = vec![cc];
        ccs.extend(m.secondary.iter().map(|&f| f as f64).filter(|&f| f != cc));
        let ppm = m.ppm;
        let rate = self.survey.rate_hz();
        let voice: Vec<f64> = m.voice.iter().map(|v| v.freq_hz as f64).collect();
        let (center, covered) = best_center(cc, &ccs, &voice, rate, self.guard_hz);
        let lo = voice.iter().chain(&ccs).copied().fold(f64::INFINITY, f64::min);
        let hi = voice.iter().chain(&ccs).copied().fold(f64::NEG_INFINITY, f64::max);
        let smartnet = m.smartnet.as_ref().and_then(|s| s.plan.as_ref());
        Some(json!({
            "type": if smartnet.is_some() { "smartnet" } else { "p25" },
            "bandplan": smartnet.map(plan_json),
            "controlChannels": ccs,
            "ppm": ppm.map(|p| (p * 100.0).round() / 100.0),
            // An RTL-SDR takes whole ppm.
            "ppmApply": ppm.map(|p| if self.rtl { p.round() } else { (p * 100.0).round() / 100.0 }),
            "gainDb": m.gain.best_db,
            "centerHz": center,
            "voiceCovered": covered,
            "voiceTotal": voice.len(),
            "spanHz": if hi > lo { hi - lo } else { 0.0 },
            "identity": crate::session::identity_json(&m.identity),
            "voiceChannels": voice,
        }))
    }
}

/// A learned SmartNet band plan in the config's terms (Trunk Recorder's names).
fn plan_json(p: &trunk_core::smartnet::plan::PlanConfig) -> Value {
    if p.name == "400_custom" {
        json!({ "bandplan": p.name, "bandplanBaseHz": p.base_hz, "bandplanSpacingHz": p.spacing_hz, "bandplanOffset": p.offset, "bandplanHighHz": p.high_hz })
    } else {
        json!({ "bandplan": p.name })
    }
}

/// A centre (off the DC spike) whose usable band holds the primary control
/// channel and as many of the voice channels (and other control channels) as
/// fit; and how many voice channels that covers.
pub fn best_center(primary: f64, ccs: &[f64], voice: &[f64], rate_hz: f64, guard_hz: f64) -> (f64, usize) {
    let w = 2.0 * usable_half_width(rate_hz, guard_hz) - 60_000.0;
    let mut all: Vec<f64> = voice.iter().chain(ccs).copied().collect();
    all.sort_by(f64::total_cmp);
    let (mut best, mut best_n) = ((primary, primary), (0usize, 0usize));
    for &lo in all.iter().filter(|&&f| f <= primary && f >= primary - w) {
        let hi = lo + w;
        let n_voice = voice.iter().filter(|&&f| f >= lo && f <= hi).count();
        let n_cc = ccs.iter().filter(|&&f| f >= lo && f <= hi).count();
        if (n_voice, n_cc) > best_n {
            best_n = (n_voice, n_cc);
            let inside: Vec<f64> = all.iter().copied().filter(|&f| f >= lo && f <= hi).collect();
            best = (inside[0], *inside.last().unwrap());
        }
    }
    let inside: Vec<f64> = all.iter().copied().filter(|&f| f >= best.0 && f <= best.1).collect();
    let pts = if inside.is_empty() { vec![primary] } else { inside };
    let center = auto_center(&pts, rate_hz, guard_hz).or_else(|| auto_center(&[primary], rate_hz, guard_hz)).unwrap_or(primary + 250_000.0);
    (center, best_n.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_covers_the_busiest_span() {
        // 2.4 MSPS: ~2.25 MHz usable. Voice spread over 851–856 MHz; the CC at 851.5.
        let voice = [851.0125e6, 851.2625e6, 851.5125e6, 852.0125e6, 852.4625e6, 855.9e6];
        let (c, n) = best_center(851.5e6, &[851.5e6], &voice, 2_400_000.0, DEFAULT_GUARD_HZ);
        assert_eq!(n, 5);
        assert!((c - 851.5e6).abs() > 20_000.0, "on DC");
        for f in &voice[..5] {
            assert!((f - c).abs() <= usable_half_width(2_400_000.0, DEFAULT_GUARD_HZ), "{f} outside {c}");
        }
    }

    #[test]
    fn request_defaults() {
        let r = Request::from_json(&json!({ "source": 1 }));
        assert_eq!(r.source, 1);
        assert!(r.bands.contains(&"800".to_string()) && !r.bands.contains(&"t-band".to_string()));
        assert!(r.find_gain);
    }
}
