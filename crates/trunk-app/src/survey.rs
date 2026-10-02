//! The first-run survey for the interface, shared by the desktop app and the
//! web build: the core [`Survey`] set up from a configured source, reported
//! as `survey` messages (a whole snapshot, ~2.5/s) and `surveySpectrum`, with
//! a suggested setup once the control channel has told enough — the control
//! channels, the ppm to set, the gain, and a centre that covers the most
//! voice channels seen.
//!
//! The platform drives it: [`SurveySession::survey`]'s commands tune the
//! radio; samples and tune confirmations go back in.

use serde_json::{json, Value};
use trunk_core::survey::{Command, GainState, Kind, Stage, Survey, SurveyConfig, BANDS};
use trunk_core::trunk::engine::Identity;

use crate::config::{auto_center, usable_half_width, Config, Source};
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
    pub bands: Vec<String>,
    /// Step the gain while monitoring (RTL-SDR).
    pub find_gain: bool,
}

impl Request {
    pub fn from_json(v: &Value) -> Request {
        let bands = v["bands"].as_array().map(|a| a.iter().filter_map(|b| b.as_str().map(str::to_string)).collect::<Vec<_>>());
        Request {
            source: v["source"].as_u64().unwrap_or(0) as usize,
            bands: bands.filter(|b| !b.is_empty()).unwrap_or_else(|| BANDS.iter().filter(|b| b.default_on).map(|b| b.id.to_string()).collect()),
            find_gain: v["findGain"].as_bool().unwrap_or(true),
        }
    }
}

/// The survey settings for source `i` of `cfg`.
pub fn survey_config(cfg: &Config, req: &Request) -> Result<SurveyConfig, String> {
    let src = cfg.sources.get(req.source).ok_or("No such source.")?;
    let mut sc = SurveyConfig { rate_hz: src.rate_hz(), bands: req.bands.clone(), ..Default::default() };
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
        // The device's range isn't known here: the widest any module offers; a
        // tune it refuses shows as a source error.
        Source::Soapy { ppm, .. } => {
            sc.ppm = *ppm;
            sc.tune_range_hz = (1e6, 6e9);
            sc.settle_s = 0.2;
        }
        Source::File { .. } => {
            let c = cfg.resolved_centers().get(req.source).copied().unwrap_or(0.0);
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
    rtl: bool,
    bands: Vec<String>,
    error: Option<String>,
    last_ms: f64,
    last_spec_ms: f64,
    changed: bool,
}

impl SurveySession {
    pub fn new(cfg: &Config, req: &Request) -> Result<SurveySession, String> {
        let sc = survey_config(cfg, req)?;
        Ok(SurveySession {
            survey: Survey::new(sc),
            source: req.source,
            rtl: matches!(cfg.sources[req.source], Source::Rtlsdr { .. }),
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

    /// A snapshot every 400 ms of wall clock (at once after a stage change),
    /// the spectrum every 150 ms.
    pub fn poll(&mut self, now_ms: f64, out: &mut Vec<Output>) {
        if now_ms - self.last_spec_ms >= 150.0 {
            self.last_spec_ms = now_ms;
            if let Some((center, bins)) = self.survey.spectrum(512) {
                let bins: Vec<f32> = bins.iter().map(|v| (v * 10.0).round() / 10.0).collect();
                out.push(Output::Text(json!({ "type": "surveySpectrum", "source": self.source, "centerHz": center, "rateHz": self.survey.rate_hz(), "bins": bins }).to_string()));
            }
        }
        if self.changed && now_ms - self.last_ms >= 400.0 {
            self.last_ms = now_ms;
            self.changed = false;
            out.push(Output::Text(self.snapshot().to_string()));
        }
    }

    pub fn snapshot(&self) -> Value {
        let s = &self.survey;
        let id = |i: &Identity| json!({ "nac": i.nac, "wacn": i.wacn, "sysId": i.sys_id, "rfss": i.rfss, "site": i.site });
        let candidates: Vec<Value> = s
            .candidates()
            .iter()
            .filter(|c| c.kind != Kind::Other || c.snr_db >= 6.0)
            .map(|c| {
                json!({
                    "freqHz": c.freq_hz.round(), "correctedHz": s.corrected(c.freq_hz).map(f64::round), "band": c.band, "snrDb": (c.snr_db * 10.0).round() / 10.0,
                    "widthHz": c.width_hz.round(), "kind": c.kind.as_str(), "frames": c.frames, "good": c.good, "bad": c.bad,
                    "modulation": c.modulation, "identity": id(&c.identity),
                    "dmr": c.dmr.map(|d| json!({ "variant": d.variant.map(|v| v.name()), "colorCode": d.color_code })),
                })
            })
            .collect();
        let monitor = s.system().map(|m| {
            json!({
                "freqHz": s.monitoring_hz().map(f64::round), "heardHz": m.heard_hz.round(), "identity": id(&m.identity), "advertisedHz": m.advertised_hz,
                "secondary": m.secondary, "adjacent": m.adjacent.iter().map(|a| json!({ "rfss": a.rfss, "site": a.site, "sysId": a.sys_id, "freqHz": a.freq_hz })).collect::<Vec<_>>(),
                "voice": m.voice.iter().map(|v| json!({ "freqHz": v.freq_hz, "grants": v.grants, "tdma": v.tdma })).collect::<Vec<_>>(),
                "idens": m.idens, "good": m.good, "bad": m.bad, "modulation": m.modulation, "snrDb": m.snr_db.map(|v| (v * 10.0).round() / 10.0),
                "offsetHz": m.offset_hz.map(f64::round), "ppm": m.ppm.map(|p| (p * 100.0).round() / 100.0),
                "gain": {
                    "state": match m.gain.state { GainState::Off => "off", GainState::Waiting => "waiting", GainState::Running => "running", GainState::Done => "done" },
                    "steps": m.gain.steps.iter().map(|g| json!({ "gainDb": g.gain_db, "snrDb": (g.snr_db * 10.0).round() / 10.0, "okRatio": g.ok_ratio, "clipped": g.clipped })).collect::<Vec<_>>(),
                    "bestDb": m.gain.best_db,
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
            "source": self.source,
            "bands": self.bands,
            "message": s.message(),
            "error": self.error,
            "progress": progress,
            "candidates": candidates,
            "monitor": monitor,
            "suggest": self.suggest(),
        })
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
        let (center, covered) = best_center(cc, &ccs, &voice, rate);
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
            "nac": m.identity.nac,
            "sysId": m.identity.sys_id,
            "wacn": m.identity.wacn,
            "rfss": m.identity.rfss,
            "site": m.identity.site,
            "voiceChannels": voice,
        }))
    }
}

/// A learned SmartNet band plan in the config's terms (Trunk Recorder's names).
fn plan_json(p: &trunk_core::smartnet::plan::PlanConfig) -> Value {
    if p.name == "400_custom" {
        json!({ "bandplan": p.name, "bandplanBase": p.base_hz, "bandplanSpacing": p.spacing_hz, "bandplanOffset": p.offset, "bandplanHigh": p.high_hz })
    } else {
        json!({ "bandplan": p.name })
    }
}

/// A centre (off the DC spike) whose usable band holds the primary control
/// channel and as many of the voice channels (and other control channels) as
/// fit; and how many voice channels that covers.
pub fn best_center(primary: f64, ccs: &[f64], voice: &[f64], rate_hz: f64) -> (f64, usize) {
    let w = 2.0 * usable_half_width(rate_hz) - 60_000.0;
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
    let center = auto_center(&pts, rate_hz).or_else(|| auto_center(&[primary], rate_hz)).unwrap_or(primary + 250_000.0);
    (center, best_n.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_covers_the_busiest_span() {
        // 2.4 MSPS: ~2.1 MHz usable. Voice spread over 851–856 MHz; the CC at 851.5.
        let voice = [851.0125e6, 851.2625e6, 851.5125e6, 852.0125e6, 852.4625e6, 855.9e6];
        let (c, n) = best_center(851.5e6, &[851.5e6], &voice, 2_400_000.0);
        assert_eq!(n, 5);
        assert!((c - 851.5e6).abs() > 20_000.0, "on DC");
        for f in &voice[..5] {
            assert!((f - c).abs() <= usable_half_width(2_400_000.0), "{f} outside {c}");
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
