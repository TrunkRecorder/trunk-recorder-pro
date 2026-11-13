//! Rules that make a capture when something happens (research/iq-corpus.md):
//! an event, a condition in CEL (the Common Expression Language), and what
//! to keep. CEL can't loop, so a rule shared on an issue is safe to paste in;
//! what CEL has no way to say (how often, how recently) is the rule's.
//!
//! The recording sends facts here and goes on: rules run on a thread of
//! their own, never on one that decodes. They only ever make local files.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::mpsc::{self, RecvTimeoutError, Sender, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cel::{Context, Program, Value as CelValue};
use serde_json::{json, Value};
use trunk_app::config::{CaptureBudget, CaptureRule, CaptureTarget, Config, Diagnostics};
use trunk_core::trunk::tape::VoiceTape;
use trunk_recorder_plugin::CallRecord;

use crate::iq_capture::{self, Asked, Request, Waiting};
use crate::runtime::{Ctx, EngineCmd};

/// A tape kept this long, waiting for a rule that asks for it (its call's end and its channel's closing race).
const TAPE_HOLD: Duration = Duration::from_secs(30);
const WANTED_HOLD: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_secs(10);
/// How long a wideband capture runs after what set it off, unless the rule says.
const DEFAULT_AFTER_S: f64 = 10.0;

/// What's known of an event, as a rule's condition sees it.
pub struct Fact {
    pub on: &'static str,
    pub facts: Value,
}

/// The events rules can be on, and an example of each one's facts: what a
/// condition may name is checked against it when a rule is saved.
fn sample(on: &str) -> Option<Value> {
    let call = json!({ "system": "x", "site": "x", "talkgroup": 1, "freq": 1, "start_time": 1.0, "call_length": 1.0, "clean_voice_pct": 1.0, "snr": 1.0, "signal": 1.0, "noise": 1.0, "freq_error": 1, "encrypted": false, "decrypted": false, "emergency": false, "analog": false, "tdma_slot": 0, "units": 1 });
    let control = json!({ "system": "x", "site": "x", "freq_hz": 1 });
    let source = json!({ "index": 0, "pct": 1.0, "per_s": 1.0 });
    Some(match on {
        "call.end" | "call.not_saved" => json!({ "call": call }),
        "control.lost" | "control.regained" => json!({ "control": control }),
        "source.clipping" | "source.drops" => json!({ "source": source }),
        "note.warning" => json!({ "note": { "system": "x", "site": "x", "text": "x" } }),
        "tick" => json!({ "site": { "system": "x", "name": "x", "good": 1, "bad": 1, "failed_pct": 1.0, "messages": 1 } }),
        "schedule" => json!({}),
        _ => return None,
    })
}

pub const EVENTS: &[&str] = &["call.end", "call.not_saved", "control.lost", "control.regained", "source.clipping", "source.drops", "note.warning", "tick", "schedule"];

fn context(facts: &Value) -> Context<'static, 'static> {
    let mut c = Context::default();
    for (k, v) in facts.as_object().into_iter().flatten() {
        let _ = c.add_variable(k.clone(), v.clone());
    }
    c
}

/// How long a duration like `30s`, `5m` or `2h` is, s.
pub fn seconds(s: &str) -> Result<f64, String> {
    let s = s.trim();
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(s.len()));
    let n: f64 = n.parse().map_err(|_| format!("{s:?} isn't a duration like 30s, 5m or 2h"))?;
    Ok(n * match unit {
        "s" | "" => 1.0,
        "m" => 60.0,
        "h" => 3600.0,
        "d" => 86400.0,
        _ => return Err(format!("{s:?} isn't a duration like 30s, 5m or 2h")),
    })
}

fn program(rule: &CaptureRule) -> Result<Program, String> {
    Program::compile(if rule.condition.trim().is_empty() { "true" } else { &rule.condition }).map_err(|e| format!("Rule {}: its condition: {e}", rule.id))
}

/// Whether `rule` can be run: what's wrong with it if not.
pub fn check(rule: &CaptureRule) -> Result<(), String> {
    let id = &rule.id;
    if id.trim().is_empty() {
        return Err("A rule needs an id.".into());
    }
    let facts = sample(&rule.on).ok_or_else(|| format!("Rule {id}: there's no event {:?} (one of: {})", rule.on, EVENTS.join(", ")))?;
    let p = program(rule)?;
    match p.execute(&context(&facts)) {
        Ok(CelValue::Bool(_)) => {}
        Ok(other) => return Err(format!("Rule {id}: its condition must be true or false, not {other:?}")),
        Err(e) => return Err(format!("Rule {id}: its condition can't run on {}: {e}", rule.on)),
    }
    if rule.capture.is_empty() {
        return Err(format!("Rule {id}: it doesn't say what to capture."));
    }
    for c in &rule.capture {
        if !matches!(c.tier.as_str(), "wideband" | "control" | "voice") {
            return Err(format!("Rule {id}: there's no tier {:?} (wideband, control, voice)", c.tier));
        }
        if c.tier == "voice" && !matches!(rule.on.as_str(), "call.end" | "call.not_saved") {
            return Err(format!("Rule {id}: a voice capture needs a call: {} isn't about one", rule.on));
        }
        for d in [&c.before, &c.after].into_iter().flatten() {
            seconds(d).map_err(|e| format!("Rule {id}: {e}"))?;
        }
    }
    for d in [rule.every.as_ref(), rule.repeat.as_ref().map(|r| &r.within), rule.limit.as_ref().map(|l| &l.cooldown)].into_iter().flatten() {
        seconds(d).map_err(|e| format!("Rule {id}: {e}"))?;
    }
    if rule.on == "schedule" && rule.every.is_none() {
        return Err(format!("Rule {id}: a schedule says how often: every, like 24h"));
    }
    Ok(())
}

/// Whether the rules can all be run (duplicates of an id can't be told apart).
pub fn check_all(d: &Diagnostics) -> Result<(), String> {
    for (i, r) in d.rules.iter().enumerate() {
        check(r)?;
        if d.rules[..i].iter().any(|o| o.id == r.id) {
            return Err(format!("Two rules are called {}.", r.id));
        }
    }
    Ok(())
}

/// The rules shipped, off until switched on.
pub fn presets() -> Vec<CaptureRule> {
    let capture = |tier: &str, before: Option<&str>, after: Option<&str>| CaptureTarget { tier: tier.into(), before: before.map(str::to_string), after: after.map(str::to_string) };
    let rule = |id: &str, on: &str, condition: &str, what: Vec<CaptureTarget>| CaptureRule { id: id.into(), enabled: true, on: on.into(), condition: condition.into(), capture: what, limit: Some(trunk_app::config::CaptureLimit { cooldown: "30m".into() }), ..Default::default() };
    let repeat = |count: u32, within: &str, by: &str| Some(trunk_app::config::CaptureRepeat { count, within: within.into(), by: by.into() });
    vec![
        rule("control-lost", "control.lost", "true", vec![capture("control", Some("60s"), None)]),
        CaptureRule { repeat: repeat(3, "2m", "site.system"), ..rule("control-failing", "tick", "site.messages > 20 && site.failed_pct > 20.0", vec![capture("control", Some("60s"), None)]) },
        rule("channel-not-in-table", "note.warning", "note.text.contains(\"not in the table\")", vec![capture("control", Some("60s"), None)]),
        CaptureRule { repeat: repeat(3, "5m", "call.freq"), ..rule("poor-voice", "call.end", "call.call_length > 3.0 && !call.encrypted && !call.analog && call.clean_voice_pct < 70.0", vec![capture("voice", None, None)]) },
        rule("no-audio", "call.not_saved", "call.call_length > 1.0 && !call.encrypted", vec![capture("voice", None, None)]),
        rule("clipping", "source.clipping", "source.pct > 1.0", vec![capture("wideband", Some("10s"), Some("10s"))]),
        CaptureRule { every: Some("24h".into()), ..rule("healthy-sample", "schedule", "true", vec![capture("wideband", None, Some("30s"))]) },
    ]
}

struct Compiled {
    rule: CaptureRule,
    program: Program,
    /// Matches lately, per `repeat.by` value.
    recent: HashMap<String, VecDeque<f64>>,
    last_fired: Option<f64>,
    next_scheduled: Option<f64>,
}

/// The rules of a recording, and what they've done.
pub struct RuleSet {
    rules: Vec<Compiled>,
    budget: CaptureBudget,
    day: i64,
    captures_today: u32,
    bytes_today: u64,
}

/// A path like `call.freq` in the facts, as text.
fn lookup(facts: &Value, path: &str) -> String {
    let v = path.split('.').fold(facts, |v, k| &v[k]);
    v.as_str().map_or_else(|| v.to_string(), str::to_string)
}

/// The system an event is about, if it's about one.
fn system_of(facts: &Value) -> Option<&str> {
    facts.as_object()?.values().find_map(|v| v["system"].as_str())
}

impl RuleSet {
    /// The enabled rules of `d`. Those that can't run are left out and logged (saving a config checks them).
    pub fn new(d: &Diagnostics) -> RuleSet {
        let rules = d
            .rules
            .iter()
            .filter(|r| r.enabled)
            .filter_map(|r| match check(r).and_then(|()| program(r)) {
                Ok(program) => Some(Compiled { rule: r.clone(), program, recent: HashMap::new(), last_fired: None, next_scheduled: None }),
                Err(e) => {
                    log::warn!("{e}");
                    None
                }
            })
            .collect();
        RuleSet { rules, budget: d.budget, day: 0, captures_today: 0, bytes_today: 0 }
    }

    /// The rules an event sets off, now (Unix s). A condition that fails to run on the facts doesn't match.
    fn event(&mut self, on: &str, facts: &Value, now: f64) -> Vec<CaptureRule> {
        let ctx = context(facts);
        let mut fired = Vec::new();
        for c in self.rules.iter_mut().filter(|c| c.rule.on == on) {
            if !c.rule.systems.is_empty() && !system_of(facts).is_some_and(|s| c.rule.systems.iter().any(|x| x == s)) {
                continue;
            }
            if !matches!(c.program.execute(&ctx), Ok(CelValue::Bool(true))) {
                continue;
            }
            if let Some(r) = &c.rule.repeat {
                let within = seconds(&r.within).unwrap_or(0.0);
                let seen = c.recent.entry(lookup(facts, &r.by)).or_default();
                seen.push_back(now);
                while seen.front().is_some_and(|t| now - t > within) {
                    seen.pop_front();
                }
                if seen.len() < r.count.max(1) as usize {
                    continue;
                }
                seen.clear();
            }
            if let Some(l) = &c.rule.limit {
                if c.last_fired.is_some_and(|t| now - t < seconds(&l.cooldown).unwrap_or(0.0)) {
                    continue;
                }
            }
            c.last_fired = Some(now);
            fired.push(c.rule.clone());
        }
        fired
    }

    /// Scheduled rules that are due.
    fn scheduled(&mut self, now: f64) -> Vec<CaptureRule> {
        let mut due = Vec::new();
        for c in self.rules.iter_mut().filter(|c| c.rule.on == "schedule") {
            let every = c.rule.every.as_deref().and_then(|e| seconds(e).ok()).unwrap_or(86400.0);
            match c.next_scheduled {
                Some(t) if now >= t => {
                    c.next_scheduled = Some(now + every);
                    due.push(c.rule.clone());
                }
                None => c.next_scheduled = Some(now + every),
                _ => {}
            }
        }
        due
    }

    /// Room for a capture of about `bytes` today? Counts it if so.
    fn afford(&mut self, bytes: u64, now: f64) -> bool {
        let day = (now / 86400.0) as i64;
        if day != self.day {
            (self.day, self.captures_today, self.bytes_today) = (day, 0, 0);
        }
        if self.captures_today >= self.budget.captures_per_day || self.bytes_today + bytes > self.budget.mb_per_day as u64 * 1_000_000 {
            return false;
        }
        self.captures_today += 1;
        self.bytes_today += bytes;
        true
    }

    fn wants(&self, on: &str) -> bool {
        self.rules.iter().any(|c| c.rule.on == on)
    }
}

/// A call's facts, as conditions see them: nothing from it is stored unless a rule keeps a capture.
pub fn call_facts(r: &CallRecord) -> Value {
    json!({ "call": {
        "system": r.short_name, "site": r.site, "talkgroup": r.talkgroup, "freq": r.freq, "start_time": r.start_time, "call_length": r.call_length,
        // (An analog call has no vocoder frames to be clean or not.)
        "clean_voice_pct": r.clean_voice_pct.unwrap_or(100.0),
        "snr": r.snr.unwrap_or(0.0), "signal": r.signal.unwrap_or(0.0), "noise": r.noise.unwrap_or(0.0), "freq_error": r.freq_error,
        "encrypted": r.encrypted, "decrypted": r.decrypted, "emergency": r.emergency, "analog": r.audio_type == "analog", "tdma_slot": r.tdma_slot, "units": r.src_list.len(),
    } })
}

/// A recent event as a fact, if a rule could be on it.
fn monitor_fact(e: &Value, labels: &[String]) -> Option<Fact> {
    let source = |e: &Value| json!({ "index": e["source"].as_str().and_then(|s| labels.iter().position(|l| l == s)).unwrap_or(0), "pct": e["pct"], "per_s": e["perS"] });
    let control = |e: &Value| json!({ "system": e["system"], "site": e["site"].as_str().unwrap_or(""), "freq_hz": e["freqHz"] });
    Some(match e["type"].as_str()? {
        "controlLost" => Fact { on: "control.lost", facts: json!({ "control": control(e) }) },
        "controlRegained" => Fact { on: "control.regained", facts: json!({ "control": control(e) }) },
        "sourceClipping" => Fact { on: "source.clipping", facts: json!({ "source": source(e) }) },
        "sourceDrops" => Fact { on: "source.drops", facts: json!({ "source": source(e) }) },
        _ => return None,
    })
}

/// What a voice channel's tape is wanted for.
enum For {
    Rule(CaptureRule),
    /// A capture asked for by hand ([`Waiting`], by its key).
    Asked(u64),
}

struct Wanted {
    freq: u64,
    of: For,
    until: Instant,
}

struct Worker {
    ctx: Arc<Ctx>,
    cfg: Config,
    rules: RuleSet,
    labels: Vec<String>,
    seen_t: f64,
    tapes: VecDeque<(Instant, VoiceTape)>,
    wanted: Vec<Wanted>,
    /// Capturing a control channel, later: (when, the rule).
    later: Vec<(Instant, CaptureRule, Option<String>)>,
    last_tick: Instant,
    /// Each site's message counts at the last tick.
    counts: BTreeMap<u16, (u64, u64)>,
    /// The engine is taping channels (for a rule, or a capture asked for), since when.
    taping: bool,
    taping_since: Instant,
}

fn ask<T>(ctx: &Ctx, cmd: impl FnOnce(Sender<T>) -> EngineCmd) -> Option<T> {
    let (done, answer) = mpsc::channel();
    ctx.tell_engine(cmd(done));
    answer.recv_timeout(Duration::from_secs(5)).ok()
}

impl Worker {
    fn now() -> f64 {
        crate::clock::unix_s()
    }

    fn fact(&mut self, f: &Fact) {
        let now = Self::now();
        let fired = self.rules.event(f.on, &f.facts, now);
        for rule in fired {
            log::info!("Capture rule {} fired on {}", rule.id, f.on);
            self.act(&rule, &f.facts);
        }
        if matches!(f.on, "call.end" | "call.not_saved") {
            self.next_call(&f.facts["call"]);
        }
    }

    /// A call ended: the voice channel of the next call is wanted by those asked
    /// for, on its system (and talkgroup), that began once they were asked.
    fn next_call(&mut self, call: &Value) {
        let Some(freq) = call["freq"].as_u64() else { return };
        let started = call["start_time"].as_f64().unwrap_or(0.0);
        let waiting = self.ctx.iq.waiting.lock().unwrap().clone();
        for w in waiting {
            let Asked::Call { system, talkgroup } = &w.asked else { continue };
            if call["system"].as_str() == Some(system) && talkgroup.is_none_or(|t| call["talkgroup"].as_u64() == Some(t as u64)) && started >= w.since.floor() {
                self.wanted.push(Wanted { freq, of: For::Asked(w.key), until: Instant::now() + WANTED_HOLD });
            }
        }
    }

    fn act(&mut self, rule: &CaptureRule, facts: &Value) {
        for target in &rule.capture {
            match target.tier.as_str() {
                "wideband" => self.wideband(rule, target),
                "control" => {
                    let after = target.after.as_deref().and_then(|a| seconds(a).ok()).unwrap_or(0.0);
                    let system = system_of(facts).map(str::to_string);
                    self.later.push((Instant::now() + Duration::from_secs_f64(after), rule.clone(), system));
                }
                "voice" => {
                    if let Some(freq) = facts["call"]["freq"].as_u64() {
                        self.wanted.push(Wanted { freq, of: For::Rule(rule.clone()), until: Instant::now() + WANTED_HOLD });
                    }
                }
                _ => {}
            }
        }
    }

    fn wideband(&mut self, rule: &CaptureRule, target: &CaptureTarget) {
        let after = target.after.as_deref().and_then(|a| seconds(a).ok()).unwrap_or(DEFAULT_AFTER_S);
        let before = target.before.as_deref().and_then(|b| seconds(b).ok()).unwrap_or(0.0);
        let bytes = iq_capture::estimate(&self.cfg, before + after);
        if !self.rules.afford(bytes, Self::now()) {
            log::info!("Capture rule {}: today's capture budget is spent", rule.id);
            return;
        }
        let req = Request { after_s: after.max(1.0), before_s: before, note: format!("Rule {}", rule.id), purpose: "rule", rule: Some(rule_json(rule)) };
        if let Err(e) = iq_capture::begin_capture(&self.ctx, &req) {
            log::info!("Capture rule {}: {e}", rule.id);
        }
    }

    fn control(&mut self, rule: &CaptureRule, system: Option<&str>) {
        let Some((tapes, live)) = ask(&self.ctx, |done| EngineCmd::IqControl { done }) else { return };
        let bytes: u64 = tapes.iter().map(|t| t.iq.len() as u64 * 8).sum();
        if tapes.is_empty() || !self.rules.afford(bytes, Self::now()) {
            return;
        }
        for t in tapes.iter().filter(|t| system.is_none_or(|s| live.status.sites.get(t.site as usize).is_some_and(|y| y.short_name == s))) {
            if let Err(e) = iq_capture::save_control(&self.ctx, &self.cfg, t, &live, iq_capture::for_rule(rule_json(rule))) {
                log::warn!("Capture rule {}: {e}", rule.id);
            }
        }
    }

    /// A control channel capture asked for, its time up: what its site's control channel had since.
    fn control_asked(&mut self, w: &Waiting, site: u16, seconds: u32) {
        let Some((tapes, live)) = ask(&self.ctx, |done| EngineCmd::IqControl { done }) else { return self.ctx.iq.failed(format!("{}: the recording didn't answer.", w.text)) };
        let mine: Vec<_> = tapes.into_iter().filter(|t| t.site == site && !t.iq.is_empty()).collect();
        if mine.is_empty() {
            return self.ctx.iq.failed(format!("{}: nothing to record. Is its control channel inside a radio's range?", w.text));
        }
        // (A DMR site's carriers are each a capture.)
        for mut t in mine {
            t.keep_last(seconds as f64);
            if let Err(e) = iq_capture::save_control(&self.ctx, &self.cfg, &t, &live, w.why()) {
                self.ctx.iq.failed(format!("{}: {e}", w.text));
            }
        }
    }

    /// Channel tapes are needed: for a rule, or a capture asked for.
    fn wants_tapes(&self) -> bool {
        self.cfg.diagnostics.needs_tapes() || !self.ctx.iq.waiting.lock().unwrap().is_empty()
    }

    /// Tapes that have come in, and those a rule has been waiting for.
    fn housekeep(&mut self) {
        let taping = self.wants_tapes();
        if taping != self.taping {
            self.ctx.tell_engine(EngineCmd::IqKeepTapes { on: taping });
            (self.taping, self.taping_since) = (taping, Instant::now());
            if !taping {
                self.tapes.clear();
            }
        }
        if self.taping {
            if let Some(new) = ask(&self.ctx, |done| EngineCmd::IqTapes { done }) {
                let at = Instant::now();
                self.tapes.extend(new.into_iter().map(|t| (at, t)));
            }
        }
        let max = self.cfg.diagnostics.budget.ram_mb as usize * 1_000_000;
        let mut held: usize = self.tapes.iter().map(|(_, t)| t.iq.len() * 8).sum();
        while self.tapes.front().is_some_and(|(at, _)| at.elapsed() > TAPE_HOLD) || held > max {
            let Some((_, t)) = self.tapes.pop_front() else { break };
            held -= t.iq.len() * 8;
        }
        let wanted = std::mem::take(&mut self.wanted);
        for w in wanted {
            // (Another call made it already, or it was given up.)
            if matches!(w.of, For::Asked(k) if !self.ctx.iq.is_waiting(k)) {
                continue;
            }
            let Some(i) = self.tapes.iter().rposition(|(_, t)| t.freq_hz as u64 == w.freq) else {
                // (Its call began before its channel was taped: the next one, then.)
                if w.until > Instant::now() {
                    self.wanted.push(w);
                }
                continue;
            };
            let (_, t) = self.tapes.remove(i).unwrap();
            match w.of {
                For::Rule(rule) => {
                    if self.rules.afford(t.iq.len() as u64 * 8, Self::now()) {
                        if let Err(e) = iq_capture::save_voice(&self.ctx, &t, iq_capture::for_rule(rule_json(&rule))) {
                            log::warn!("Capture rule {}: {e}", rule.id);
                        }
                    }
                }
                For::Asked(key) => {
                    let Some(asked) = self.ctx.iq.take_waiting(key) else { continue };
                    if let Err(e) = iq_capture::save_voice(&self.ctx, &t, asked.why()) {
                        self.ctx.iq.failed(format!("{}: {e}", asked.text));
                    }
                }
            }
        }
        // Control channel captures asked for whose time is up: from when they were asked, or when taping began after.
        let since = |w: &Waiting| w.started.max(self.taping_since).elapsed().as_secs_f64();
        let due: Vec<Waiting> = self.ctx.iq.waiting.lock().unwrap().iter().filter(|w| matches!(w.asked, Asked::Control { seconds, .. } if since(w) >= seconds as f64)).cloned().collect();
        for w in due {
            if let (Some(w), Asked::Control { site, seconds }) = (self.ctx.iq.take_waiting(w.key), w.asked.clone()) {
                self.control_asked(&w, site, seconds);
            }
        }
        let later = std::mem::take(&mut self.later);
        for (at, rule, system) in later {
            if at <= Instant::now() {
                self.control(&rule, system.as_deref());
            } else {
                self.later.push((at, rule, system));
            }
        }
        for rule in self.rules.scheduled(Self::now()) {
            log::info!("Capture rule {} is due", rule.id);
            self.act(&rule, &json!({}));
        }
        // What the monitor said since last time.
        let recent = self.ctx.shared.recent_events();
        let (seen, last) = (self.seen_t, recent.iter().filter_map(|e| e["t"].as_f64()).fold(self.seen_t, f64::max));
        for e in recent.iter().filter(|e| e["t"].as_f64().is_some_and(|t| t > seen)) {
            if let Some(f) = monitor_fact(e, &self.labels) {
                self.fact(&f);
            }
        }
        self.seen_t = last;
        if self.rules.wants("tick") && self.last_tick.elapsed() >= TICK {
            self.last_tick = Instant::now();
            self.tick();
        }
    }

    /// Each site's decoding over the last interval, as a fact.
    fn tick(&mut self) {
        let Some(live) = ask(&self.ctx, |done| EngineCmd::IqLive { done }) else { return };
        for s in &live.status.sites {
            let (g, b) = self.counts.insert(s.site, (s.good, s.bad)).unwrap_or((s.good, s.bad));
            let (good, bad) = (s.good.saturating_sub(g), s.bad.saturating_sub(b));
            let messages = good + bad;
            let failed_pct = if messages > 0 { bad as f64 / messages as f64 * 100.0 } else { 0.0 };
            self.fact(&Fact { on: "tick", facts: json!({ "site": { "system": s.short_name, "name": s.site_name, "good": good, "bad": bad, "failed_pct": failed_pct, "messages": messages } }) });
        }
    }
}

fn rule_json(rule: &CaptureRule) -> Value {
    json!({ "id": rule.id, "on": rule.on, "if": rule.condition })
}

/// Start the session's rules, and what makes the captures asked for by hand that wait on the air: where to send it facts.
pub fn spawn(ctx: Arc<Ctx>, cfg: &Config) -> Option<SyncSender<Fact>> {
    let rules = RuleSet::new(&cfg.diagnostics);
    let (tx, rx) = mpsc::sync_channel::<Fact>(256);
    let labels = cfg.sources.iter().map(|s| s.label()).collect();
    let mut w = Worker { ctx, cfg: cfg.clone(), rules, labels, seen_t: crate::clock::unix_s(), tapes: VecDeque::new(), wanted: Vec::new(), later: Vec::new(), last_tick: Instant::now(), counts: BTreeMap::new(), taping: cfg.diagnostics.needs_tapes(), taping_since: Instant::now() };
    let spawned = std::thread::Builder::new().name("iq-rules".into()).spawn(move || loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(f) => w.fact(&f),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        w.housekeep();
    });
    match spawned {
        Ok(_) => Some(tx),
        Err(e) => {
            log::warn!("Capture rules not started: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trunk_app::config::{CaptureLimit, CaptureRepeat};

    fn rule(on: &str, condition: &str) -> CaptureRule {
        CaptureRule { id: "r".into(), enabled: true, on: on.into(), condition: condition.into(), capture: vec![CaptureTarget { tier: "control".into(), before: None, after: None }], ..Default::default() }
    }

    fn set(rules: Vec<CaptureRule>) -> RuleSet {
        RuleSet::new(&Diagnostics { rules, ..Default::default() })
    }

    fn call(clean: f64, freq: u64) -> Value {
        json!({ "call": { "system": "x", "freq": freq, "call_length": 9.0, "clean_voice_pct": clean, "encrypted": false, "analog": false } })
    }

    #[test]
    fn every_preset_can_run() {
        for p in presets() {
            check(&p).unwrap_or_else(|e| panic!("{e}"));
        }
    }

    #[test]
    fn a_rule_that_cant_run_is_refused_when_saved() {
        assert!(check(&rule("call.end", "call.nope > 1.0")).unwrap_err().contains("can't run"), "an unknown field");
        assert!(check(&rule("call.end", "call.freq +")).unwrap_err().contains("condition"), "a syntax error");
        assert!(check(&rule("call.end", "call.freq")).unwrap_err().contains("true or false"));
        assert!(check(&rule("nothing", "true")).unwrap_err().contains("no event"));
        let mut r = rule("control.lost", "true");
        r.capture[0].tier = "voice".into();
        assert!(check(&r).unwrap_err().contains("needs a call"));
        r.capture[0].tier = "elsewhere".into();
        assert!(check(&r).unwrap_err().contains("no tier"));
        r = rule("schedule", "true");
        assert!(check(&r).unwrap_err().contains("how often"));
        r.every = Some("soon".into());
        assert!(check(&r).unwrap_err().contains("duration"));
        assert!(check_all(&Diagnostics { rules: vec![rule("tick", "true"), rule("tick", "true")], ..Default::default() }).unwrap_err().contains("Two rules"));
    }

    #[test]
    fn a_condition_decides_and_one_that_fails_doesnt_match() {
        let mut s = set(vec![rule("call.end", "call.clean_voice_pct < 70.0 && !call.encrypted")]);
        assert_eq!(s.event("call.end", &call(50.0, 1), 0.0).len(), 1);
        assert!(s.event("call.end", &call(90.0, 1), 1.0).is_empty());
        assert!(s.event("control.lost", &call(50.0, 1), 2.0).is_empty(), "another event");
        assert!(s.event("call.end", &json!({ "call": { "clean_voice_pct": 50.0 } }), 3.0).is_empty(), "no encrypted field: an error, not a match");
    }

    #[test]
    fn repeat_counts_within_its_window_by_its_key() {
        let mut r = rule("call.end", "true");
        r.repeat = Some(CaptureRepeat { count: 2, within: "1m".into(), by: "call.freq".into() });
        let mut s = set(vec![r]);
        assert!(s.event("call.end", &call(1.0, 1), 0.0).is_empty());
        assert!(s.event("call.end", &call(1.0, 2), 1.0).is_empty(), "another frequency counts apart");
        assert_eq!(s.event("call.end", &call(1.0, 1), 2.0).len(), 1);
        assert!(s.event("call.end", &call(1.0, 1), 3.0).is_empty(), "the count starts over");
        assert!(s.event("call.end", &call(1.0, 1), 100.0).is_empty(), "the first has left the window");
    }

    #[test]
    fn a_cooldown_and_a_system_list_hold_a_rule_back() {
        let mut r = rule("call.end", "true");
        r.limit = Some(CaptureLimit { cooldown: "10s".into() });
        r.systems = vec!["x".into()];
        let mut s = set(vec![r]);
        assert_eq!(s.event("call.end", &call(1.0, 1), 0.0).len(), 1);
        assert!(s.event("call.end", &call(1.0, 1), 5.0).is_empty());
        assert_eq!(s.event("call.end", &call(1.0, 1), 11.0).len(), 1);
        assert!(s.event("call.end", &json!({ "call": { "system": "y" } }), 40.0).is_empty());
    }

    #[test]
    fn the_budget_is_per_day() {
        let mut s = RuleSet::new(&Diagnostics { budget: CaptureBudget { captures_per_day: 2, mb_per_day: 10, ..Default::default() }, ..Default::default() });
        assert!(s.afford(1_000_000, 86400.0) && s.afford(1_000_000, 86401.0));
        assert!(!s.afford(1, 86402.0), "two a day");
        assert!(s.afford(9_000_000, 2.0 * 86400.0), "a new day");
        s.captures_today = 0;
        assert!(!s.afford(2_000_000, 2.0 * 86400.0), "ten megabytes a day");
    }

    #[test]
    fn schedules_wait_for_their_first_interval() {
        let mut r = rule("schedule", "true");
        r.every = Some("10s".into());
        let mut s = set(vec![r]);
        assert!(s.scheduled(0.0).is_empty());
        assert!(s.scheduled(5.0).is_empty());
        assert_eq!(s.scheduled(10.0).len(), 1);
        assert!(s.scheduled(15.0).is_empty());
    }

    #[test]
    fn durations_read_as_people_write_them() {
        assert_eq!((seconds("30s").unwrap(), seconds("5m").unwrap(), seconds("2h").unwrap(), seconds("1d").unwrap(), seconds("90").unwrap()), (30.0, 300.0, 7200.0, 86400.0, 90.0));
        assert!(seconds("soon").is_err() && seconds("5x").is_err());
    }

    #[test]
    fn a_monitor_event_names_a_radio_by_number() {
        let f = monitor_fact(&json!({ "type": "sourceClipping", "source": "RTL-SDR SN 91", "pct": 2.5 }), &["RTL-SDR SN 200".to_string(), "RTL-SDR SN 91".to_string()]).unwrap();
        assert_eq!((f.on, &f.facts["source"]["index"], &f.facts["source"]["pct"]), ("source.clipping", &json!(1), &json!(2.5)));
        assert!(monitor_fact(&json!({ "type": "tgFirstSeen" }), &[]).is_none());
    }
}
