//! Page alerts: a Trunk Recorder Pro plugin that takes over from Tone Board.
//! TRP finds the tones in every call; this names each page by your agency
//! list (Tone Board's, with its timing rules), alerts (a sound and the
//! agency's name spoken on this computer), and sends the call to Rdio
//! Scanner under the agency's talkgroup, with Multi-Station sets on a system
//! of their own.

mod agencies;
mod join;
mod rdio;
mod speak;
mod timings;
mod unknown;
mod toneboard;

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use trunk_recorder_plugin::{
    Action, Attempt, CallInfo, CallQueue, ConcludedCall, Host, Outcome, Manifest, NoConfig, Page, Plugin, QueueOptions, Reply, Setup, State, TonePattern, TonePatternEvent, format, topic,
};

use crate::agencies::{Agency, speak_flag};
use crate::speak::{Say, Speaker};
use crate::toneboard::expand;

// ── settings ─────────────────────────────────────────────────────────────────

/// A channel pages come in on: the TRP talkgroup it is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
#[schemars(title = "Channel", extend("x-title" = "name", "x-summary" = ["{freqMhz} MHz", "{follow}", "TG {talkgroup}", "system {rdioSystem}", "{send}"]))]
pub struct Channel {
    /// Name
    pub name: String,
    /// Frequency, MHz
    ///
    /// What tone sets name it by in their Channel (453.900).
    pub freq_mhz: f64,
    /// In TRP
    ///
    /// System short name / talkgroup: `UHF/1`.
    pub follow: String,
    /// Rdio Scanner talkgroup
    ///
    /// For unknown tones and tone sets without a Unit ID. Empty: not sent.
    pub talkgroup: Option<u32>,
    /// Rdio Scanner system
    ///
    /// Pages on this channel go to this system (a county's own). Empty: the main one, below.
    pub rdio_system: Option<u32>,
    /// Send its pages
    ///
    /// Its pages go to Rdio Scanner even while Send pages to Rdio Scanner is off (a county Tone Board doesn't send).
    pub send: bool,
}

impl Config {
    /// Whether channel `ch`'s pages go to Rdio Scanner: every channel's, or this one's.
    pub fn sends(&self, ch: usize) -> bool {
        !self.rdio_server.trim().is_empty() && !self.rdio_api_key.trim().is_empty() && (self.send_pages || self.channels.get(ch).is_some_and(|c| c.send))
    }

    /// The Rdio Scanner system channel `ch`'s pages go to.
    pub fn system_of(&self, ch: usize) -> u32 {
        self.channels.get(ch).and_then(|c| c.rdio_system).filter(|s| *s > 0).unwrap_or(self.rdio_system)
    }
}

/// Agencies paged together: a row alerts (and goes to Rdio Scanner once more) when every one is paged in one transmission.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
#[schemars(title = "Multi-Station row", extend("x-title" = "name", "x-summary" = ["{agencies}", "TG {talkgroup}", "{alert}", "{sound}"]))]
#[schemars(extend("x-actions" = [
    { "id": "testMulti", "label": "Test", "hint": "Sends one test call on its talkgroup, patched with its tone sets': select it and them in Rdio Scanner and ScanFire, and it should play once" }
]))]
pub struct MultiStation {
    /// Name
    pub name: String,
    /// Agencies
    ///
    /// Their names, as on the agency list (up to four).
    pub agencies: Vec<String>,
    /// Rdio Scanner talkgroup
    pub talkgroup: u32,
    /// Speak
    ///
    /// Sound and name on this computer's speakers.
    #[serde(deserialize_with = "speak_flag")]
    pub alert: bool,
    /// Sound
    ///
    /// Empty, `beep`, or a file in the sounds folder.
    pub sound: String,
}

impl Default for MultiStation {
    fn default() -> Self {
        MultiStation { name: String::new(), agencies: Vec::new(), talkgroup: 0, alert: true, sound: "beep".into() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
#[schemars(extend("x-actions" = [
    { "id": "fillAll", "label": "Fill timings", "hint": "Every tone set's empty A length, gap and B length, from the pages TRP has recorded" },
    { "id": "addCsv", "label": "Add from CSV", "hint": "Adds the tone sets of the CSV named in Import from (a county's list) to these: those already here, by name, are left as they are" },
    { "id": "import", "label": "Import from Tone Board", "hint": "Its channels and agency list, from the folder or CSV below: replaces both here" },
    { "id": "testVoice", "label": "Test the voice", "hint": "Speaks a line on this computer's speakers" }
]))]
pub struct Config {
    /// Tone tolerance, %
    ///
    /// How far a tone heard may be from an agency's and still be it.
    #[schemars(range(min = 0.3, max = 5.0))]
    pub tolerance_pct: f64,
    /// Speak on this computer
    ///
    /// Off: no sounds or names here at all (tone sets' Speak switches aside).
    pub speak: bool,
    /// Words
    ///
    /// {agency} and {channel} are filled in.
    pub speak_text: String,
    /// Voice command
    ///
    /// Empty: Piper (Tone Board's), else espeak-ng, played here. Or a shell command of your own, with {text}.
    pub voice_command: String,
    /// Sounds folder
    pub sounds_dir: String,
    /// Join the dispatch after, s
    ///
    /// A call on the page's channel starting within this long after it goes up in the page's recording. 0: off.
    #[schemars(range(min = 0, max = 60))]
    pub follow_on_s: u32,
    /// Send pages to Rdio Scanner
    ///
    /// Off: only the Test buttons send (while Tone Board still uploads). On: every page goes up: turn Tone Board's uploads off.
    pub send_pages: bool,
    /// Rdio Scanner server
    ///
    /// Its web address (http://<your server>:3000). Empty: nothing is sent.
    pub rdio_server: String,
    /// Rdio Scanner API key
    #[schemars(extend("x-secret" = true))]
    pub rdio_api_key: String,
    /// Rdio Scanner system ID
    ///
    /// The system for pages: the tone sets' talkgroups, the channels' and the Multi-Station rows' are all in it.
    pub rdio_system: u32,
    /// Each tone set is its own talkgroup
    ///
    /// A page is on the talkgroups numbered with its tone sets' Unit IDs. Off: on its channel's talkgroup. Either way it goes up once, with every talkgroup it's on as a patch, so it plays once.
    pub own_talkgroup: bool,
    /// Send unknown tones
    ///
    /// Pages that match no agency go under their channel's talkgroup.
    pub send_unknown: bool,
    /// Unknown tones unit
    pub unknown_unit: Option<u32>,
    /// Save unknown pages
    ///
    /// A page whose tones match no tone set: its recording (the dispatch after it joined on) and a note of its tones go in the folder below, to listen to later.
    pub save_unknown: bool,
    /// Unknown pages folder
    ///
    /// A folder for each day, and index.csv listing them all.
    pub unknown_dir: String,
    /// Keep unknown pages, days
    ///
    /// Older days' folders go. 0: kept for ever.
    pub unknown_keep_days: u32,
    /// Import from
    ///
    /// Tone Board's folder (its channels and the agency list it uses) or an agency CSV.
    pub import_from: String,
    // The lists last: TRP puts the settings card above them.
    /// Channels
    pub channels: Vec<Channel>,
    /// Tone sets
    pub agencies: Vec<Agency>,
    /// Multi-Station
    pub multi_station: Vec<MultiStation>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            tolerance_pct: 1.5,
            channels: Vec::new(),
            agencies: Vec::new(),
            multi_station: Vec::new(),
            speak: true,
            speak_text: "{agency}".into(),
            voice_command: String::new(),
            sounds_dir: "~/toneboard-sounds".into(),
            follow_on_s: 15,
            send_pages: false,
            rdio_server: String::new(),
            rdio_api_key: String::new(),
            rdio_system: 0,
            own_talkgroup: true,
            send_unknown: true,
            unknown_unit: None,
            save_unknown: false,
            unknown_dir: "~/page-alerts-unknown".into(),
            unknown_keep_days: 60,
            import_from: "~/.local/share/toneboard".into(),
        }
    }
}

// ── the agency list, ready to match ──────────────────────────────────────────

/// The settings, with each channel's TRP talkgroup and each agency's channels worked out.
pub struct Lists {
    pub cfg: Config,
    /// Each channel's (system short name in lower case, or "" for any; talkgroup).
    follow: Vec<Option<(String, u32)>>,
    /// Each agency's channels (None: any).
    on: Vec<Option<Vec<usize>>>,
    /// Agencies whose channel isn't one of the channels.
    pub orphans: Vec<String>,
}

fn parse_follow(s: &str) -> Option<(String, u32)> {
    let s = s.trim();
    match s.rsplit_once('/') {
        Some((sys, tg)) => Some((sys.trim().to_lowercase(), tg.trim().parse().ok()?)),
        None => Some((String::new(), s.parse().ok()?)),
    }
}

impl Lists {
    pub fn new(cfg: Config) -> Lists {
        let follow: Vec<_> = cfg.channels.iter().map(|c| parse_follow(&c.follow)).collect();
        let mut orphans = Vec::new();
        let on = cfg
            .agencies
            .iter()
            .map(|a| {
                if a.channel.trim().is_empty() || a.channel.trim().eq_ignore_ascii_case("any") {
                    return None;
                }
                let mut v = Vec::new();
                for part in a.channel.split('/') {
                    let p = part.trim();
                    let mhz: Option<f64> = p.parse().ok();
                    for (k, c) in cfg.channels.iter().enumerate() {
                        let hit = match mhz {
                            Some(m) => (c.freq_mhz - m).abs() < 0.002,
                            None => c.name.trim().eq_ignore_ascii_case(p),
                        };
                        if hit && !v.contains(&k) {
                            v.push(k);
                        }
                    }
                }
                if v.is_empty() {
                    orphans.push(a.name.clone());
                }
                Some(v)
            })
            .collect();
        Lists { cfg, follow, on, orphans }
    }

    /// The channel a call on `short_name`'s `talkgroup` is.
    pub fn channel(&self, short_name: &str, talkgroup: u32) -> Option<usize> {
        let sys = short_name.to_lowercase();
        self.follow.iter().position(|f| f.as_ref().is_some_and(|(s, tg)| *tg == talkgroup && (s.is_empty() || *s == sys)))
    }

    /// Channel `k`'s (system short name in lower case, or ""; talkgroup).
    pub fn follows(&self, k: usize) -> Option<(String, u32)> {
        self.follow.get(k).cloned().flatten()
    }

    pub fn on_channel(&self, a: usize, ch: usize) -> bool {
        self.agency_on(a, ch)
    }

    fn agency_on(&self, a: usize, ch: usize) -> bool {
        self.on[a].as_ref().is_none_or(|v| v.contains(&ch))
    }

    /// The agency `p` is on channel `ch` (the closest, when several fit), with `beeps` counting
    /// the beeps of each agency's tone heard so far in the call.
    pub fn agency(&self, ch: usize, p: &TonePattern, beeps: &mut HashMap<usize, u32>) -> Option<usize> {
        let tol = self.cfg.tolerance_pct.clamp(0.1, 10.0) / 100.0;
        let mut best: Option<(usize, f64)> = None;
        for (i, a) in self.cfg.agencies.iter().enumerate() {
            if !self.agency_on(i, ch) {
                continue;
            }
            let mut n = 0;
            if a.beeps.is_some() && p.tones.iter().all(|t| (t - a.tone_a).abs() <= a.tone_a * tol) && !p.tones.is_empty() {
                let add = match p.pattern.as_str() {
                    "pulsed" => p.cycles.max(1),
                    "sequence" => p.tones.len() as u32,
                    _ => 1,
                };
                let c = beeps.entry(i).or_insert(0);
                *c += add;
                n = *c;
            }
            if let Some(e) = a.fits(p, tol, n)
                && best.is_none_or(|(_, b)| e < b)
            {
                best = Some((i, e));
            }
        }
        best.map(|(i, _)| i)
    }

    /// The Multi-Station rows whose agencies are all in `paged`.
    pub fn multi(&self, paged: &[usize]) -> Vec<usize> {
        let names: HashSet<String> = paged.iter().map(|&i| self.cfg.agencies[i].name.trim().to_lowercase()).collect();
        self.cfg
            .multi_station
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.agencies.is_empty() && m.agencies.iter().all(|a| names.contains(&a.trim().to_lowercase())))
            .map(|(k, _)| k)
            .collect()
    }
}

/// A page's tones worth naming (two tones, a long tone, beeps): not a warble, digits or Morse.
fn pagelike(p: &TonePattern) -> bool {
    matches!(p.pattern.as_str(), "two_tone" | "long" | "beep" | "pulsed" | "sequence")
}

/// What a call's tones were: the agencies paged (in order, once each) and whether unknown tones came too.
pub fn call_pages(lists: &Lists, ch: usize, patterns: &[TonePattern]) -> (Vec<usize>, bool) {
    let mut beeps = HashMap::new();
    let mut paged = Vec::new();
    let mut unknown = false;
    for p in patterns.iter().filter(|p| pagelike(p)) {
        match lists.agency(ch, p, &mut beeps) {
            Some(i) if !paged.contains(&i) => paged.push(i),
            Some(_) => {}
            None => unknown |= matches!(p.pattern.as_str(), "two_tone" | "long"),
        }
    }
    (paged, unknown)
}

/// The tones in a call that match no tone set (two tones or a long one: a page, not beeps or a warble).
pub fn unknown_patterns<'a>(lists: &Lists, ch: usize, patterns: &'a [TonePattern]) -> Vec<&'a TonePattern> {
    let mut beeps = HashMap::new();
    patterns.iter().filter(|p| pagelike(p) && matches!(p.pattern.as_str(), "two_tone" | "long")).filter(|p| lists.agency(ch, p, &mut beeps).is_none()).collect()
}

/// Where a call with these pages goes in Rdio Scanner: once, on one talkgroup, with every other
/// talkgroup it's on as a patch (each tone set's, its channel's, its Multi-Station rows'), so it plays
/// once for anyone listening to any or all of them. The main talkgroup is a Multi-Station row's when
/// one fired, else the first tone set's.
pub fn upload(lists: &Lists, ch: usize, paged: &[usize], unknown: bool) -> Option<rdio::Upload> {
    let c = &lists.cfg;
    let chan = &c.channels[ch];
    if paged.is_empty() && !(unknown && c.send_unknown) {
        return None;
    }
    let ag = |i: usize| &c.agencies[i];
    let multi: Vec<&MultiStation> = lists.multi(paged).into_iter().map(|k| &c.multi_station[k]).filter(|m| m.talkgroup > 0).collect();
    let units: Vec<u32> = if c.own_talkgroup { paged.iter().filter_map(|&i| ag(i).unit).collect() } else { Vec::new() };
    let without_unit = paged.iter().any(|&i| ag(i).unit.is_none());
    let on_channel = !c.own_talkgroup || without_unit || (unknown && c.send_unknown);
    let mut tgs: Vec<u32> = Vec::new();
    for tg in multi.iter().map(|m| m.talkgroup).chain(units).chain(chan.talkgroup.filter(|_| on_channel)) {
        if !tgs.contains(&tg) {
            tgs.push(tg);
        }
    }
    let (&main, patches) = tgs.split_first()?;
    let names: Vec<&str> = if !multi.is_empty() { multi.iter().map(|m| m.name.as_str()).collect() } else if !paged.is_empty() { paged.iter().map(|&i| ag(i).name.as_str()).collect() } else { vec![chan.name.as_str()] };
    let mut label = names.join(" + ");
    if label.chars().count() > 120 {
        label = label.chars().take(117).collect::<String>() + "...";
    }
    let source = paged.iter().find_map(|&i| ag(i).unit).or(c.unknown_unit);
    Some(rdio::Upload { system: c.system_of(ch), talkgroup: main, label, source, patches: patches.to_vec() })
}

// ── the plugin ───────────────────────────────────────────────────────────────

/// What a call in progress has paged so far.
#[derive(Default)]
struct OnCall {
    paged: Vec<usize>,
    beeps: HashMap<usize, u32>,
    multi: HashSet<usize>,
    seen: Instant2,
}

struct Instant2(Instant);
impl Default for Instant2 {
    fn default() -> Self {
        Instant2(Instant::now())
    }
}

struct Shared {
    host: Host,
    data_dir: PathBuf,
    capture_dir: PathBuf,
    lists: Mutex<Arc<Lists>>,
    last: Mutex<String>,
}

impl Shared {
    fn lists(&self) -> Arc<Lists> {
        self.lists.lock().unwrap().clone()
    }
    fn status(&self) {
        let l = self.lists();
        let c = &l.cfg;
        let last = self.last.lock().unwrap().clone();
        let mut msg = format!("{} tone sets on {} channels{}{}", c.agencies.len(), c.channels.len(), if c.speak { "" } else { " · speaking off" }, if c.send_pages { String::new() } else {
            let some: Vec<&str> = c.channels.iter().filter(|x| x.send).map(|x| x.name.as_str()).collect();
            if some.is_empty() { " · not sending pages (tests only)".to_string() } else { format!(" · sending only {}", some.join(", ")) }
        });
        if !last.is_empty() {
            msg.push_str(&format!(" · last: {last}"));
        }
        // Each channel it sends needs a system: its own, or the main one.
        let sent = |k: usize| c.send_pages || c.channels[k].send;
        let rdio_ok = c.rdio_server.trim().is_empty() || (!c.rdio_api_key.trim().is_empty() && (0..c.channels.len()).all(|k| !sent(k) || c.system_of(k) > 0));
        if c.channels.is_empty() || c.agencies.is_empty() {
            self.host.status(State::Warning, "No channels or agencies yet: Import from Tone Board, or add them below");
        } else if !rdio_ok {
            self.host.status(State::Warning, format!("{msg} · Rdio Scanner needs its API key, and a system ID for every channel it sends"));
        } else {
            self.host.status(State::Ok, msg);
        }
    }
}

struct PageAlerts {
    sh: Arc<Shared>,
    speaker: Speaker,
    calls: HashMap<u64, OnCall>,
    queue: Arc<Mutex<CallQueue>>,
    joiner: Arc<Mutex<join::Joiner>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

/// The pages whose wait for their dispatch is over (all: `all`) go to the queue.
fn flush_due(sh: &Shared, joiner: &Mutex<join::Joiner>, queue: &Mutex<CallQueue>, all: bool) {
    let window = sh.lists().cfg.follow_on_s as i64 * 1000;
    let due = joiner.lock().unwrap().due(window, join::now_ms(), all);
    for w in due {
        let call = join::ready(w, &sh.data_dir.join("joined"), &sh.host);
        finish(sh, queue, call);
    }
}

/// A page's call, its wait for the dispatch over: kept when it's unknown, and sent (or reported done).
fn finish(sh: &Shared, queue: &Mutex<CallQueue>, call: ConcludedCall) {
    let l = sh.lists();
    let c = &l.cfg;
    let Some(ch) = l.channel(&call.call.short_name, call.call.talkgroup) else {
        sh.host.call_result(&call.path, Outcome::Skipped, "not a paging channel", "");
        return;
    };
    let (paged, unknown) = call_pages(&l, ch, &call.call.tone_patterns);
    if c.save_unknown {
        let pats = unknown_patterns(&l, ch, &call.call.tone_patterns);
        if !pats.is_empty() {
            let named: Vec<String> = paged.iter().map(|&i| c.agencies[i].name.clone()).collect();
            match unknown::save(&c.unknown_dir, &c.channels[ch].name, &call, &pats, &named) {
                Ok(f) => sh.host.info(format!("Unknown tones kept: {}", f.display())),
                Err(e) => sh.host.warn(format!("Couldn't keep the unknown page: {e}")),
            }
            unknown::tidy(&c.unknown_dir, c.unknown_keep_days);
        }
    }
    if c.sends(ch) && upload(&l, ch, &paged, unknown).is_some() {
        queue.lock().unwrap().push(call);
    } else {
        sh.host.call_result(&call.path, Outcome::Skipped, "not sent to Rdio Scanner", "");
    }
}

fn now_hm() -> String {
    // Local time without a time zone crate: date(1).
    std::process::Command::new("date").arg("+%H:%M").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

fn fill(text: &str, agency: &str, channel: &str) -> String {
    text.replace("{agency}", agency).replace("{channel}", channel)
}

impl PageAlerts {
    fn alert(&self, l: &Lists, sound: &str, words: &str) {
        let c = &l.cfg;
        if !sound.trim().is_empty() {
            self.speaker.say(Say::Sound { sound: sound.to_string(), dir: c.sounds_dir.clone() });
        }
        if c.speak && !words.trim().is_empty() {
            self.speaker.say(Say::Words { text: words.to_string(), command: c.voice_command.clone() });
        }
    }

    fn log_page(&self, line: serde_json::Value) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.sh.data_dir.join("pages.jsonl")) {
            let _ = writeln!(f, "{line}");
        }
    }
}

impl Plugin for PageAlerts {
    type Config = Config;
    type SystemConfig = NoConfig;

    fn manifest() -> Manifest {
        Manifest {
            name: "Page alerts".into(),
            subscribe: vec![topic::TONE_PATTERN.into(), topic::CALL_START.into(), topic::CALL_CONCLUDED.into()],
            // The WAV to join a page's calls by.
            audio_formats: vec![format::M4A.into(), format::WAV.into()],
            page: Some(Page { title: "Pages".into() }),
            live_settings: true,
            ..trunk_recorder_plugin::manifest!()
        }
    }

    fn start(host: Host, setup: Setup<Config, NoConfig>) -> Result<Self, String> {
        let lists = Lists::new(setup.config.clone());
        warn_orphans(&host, &lists);
        let sh = Arc::new(Shared { host: host.clone(), data_dir: setup.data_dir.clone(), capture_dir: setup.capture_dir.clone(), lists: Mutex::new(Arc::new(lists)), last: Mutex::new(String::new()) });
        let h2 = host.clone();
        let speaker = Speaker::start(setup.data_dir.clone(), move |e| h2.warn(e));
        let sq = sh.clone();
        let opts = QueueOptions { noun: "upload", endpoint: Some("Rdio Scanner".into()), retry_after: [10, 30, 120, 600].map(Duration::from_secs).to_vec(), ..QueueOptions::saved_in(&setup.data_dir) };
        let queue = CallQueue::start(host.clone(), opts, move |call| upload_call(&sq, call));
        join::tidy(&setup.data_dir.join("joined"));
        let queue = Arc::new(Mutex::new(queue));
        let joiner = Arc::new(Mutex::new(join::Joiner::default()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let (sh, joiner, queue, stop) = (sh.clone(), joiner.clone(), queue.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    flush_due(&sh, &joiner, &queue, false);
                }
            });
        }
        sh.status();
        Ok(PageAlerts { sh, speaker, calls: HashMap::new(), queue, joiner, stop })
    }

    fn tone_pattern(&mut self, t: TonePatternEvent) {
        let l = self.sh.lists();
        let Some(ch) = l.channel(&t.short_name, t.talkgroup) else { return };
        if !pagelike(&t.pattern) {
            return;
        }
        // Forget calls long over.
        // One page per channel: tone sets heard until it's quiet a while (another transmission's
        // included, as its upload joins them), so a Multi-Station row fires across them.
        let quiet = Duration::from_secs(l.cfg.follow_on_s as u64 + 30);
        self.calls.retain(|_, c| c.seen.0.elapsed() < quiet);
        let oc = self.calls.entry(if l.cfg.follow_on_s > 0 { ch as u64 } else { t.call_num }).or_default();
        oc.seen = Instant2::default();
        let chan = l.cfg.channels[ch].name.clone();
        match l.agency(ch, &t.pattern, &mut oc.beeps) {
            Some(i) if !oc.paged.contains(&i) => {
                oc.paged.push(i);
                let a = &l.cfg.agencies[i];
                let alerts = a.alert;
                self.sh.host.info(format!("PAGE {chan}: {} — {}{}", a.name, t.pattern.text, if alerts { " (alert)" } else { "" }));
                *self.sh.last.lock().unwrap() = format!("{} ({chan}) {}", a.name, now_hm());
                let multi: Vec<usize> = l.multi(&oc.paged).into_iter().filter(|k| !oc.multi.contains(k)).collect();
                oc.multi.extend(multi.iter().copied());
                self.log_page(serde_json::json!({ "time": t.time, "channel": chan, "agency": a.name, "unit": a.unit, "text": t.pattern.text, "lengths": t.pattern.lengths, "len": t.pattern.len, "call": t.call_num, "alert": alerts }));
                if alerts {
                    let (sound, words) = (a.sound.clone(), fill(&l.cfg.speak_text, &a.name, &chan));
                    self.alert(&l, &sound, &words);
                }
                for k in multi {
                    let m = &l.cfg.multi_station[k];
                    self.sh.host.info(format!("MULTI-STATION {chan}: {}", m.name));
                    if m.alert {
                        let sound = m.sound.clone();
                        self.alert(&l, &sound, &fill(&l.cfg.speak_text, &m.name, &chan));
                    }
                }
                self.sh.status();
            }
            Some(_) => {}
            None if matches!(t.pattern.pattern.as_str(), "two_tone" | "long") => {
                self.sh.host.info(format!("UNKNOWN {chan}: {}", t.pattern.text));
                self.log_page(serde_json::json!({ "time": t.time, "channel": chan, "agency": null, "text": t.pattern.text, "call": t.call_num }));
            }
            None => {}
        }
    }

    fn call_start(&mut self, call: CallInfo) {
        let l = self.sh.lists();
        if let Some(ch) = l.channel(&call.short_name, call.talkgroup) {
            self.joiner.lock().unwrap().started(ch, call.id as u64, l.cfg.follow_on_s as i64 * 1000, join::now_ms());
        }
    }

    fn call_concluded(&mut self, call: ConcludedCall) {
        let l = self.sh.lists();
        let c = &l.cfg;
        let skip = |why: &str| self.sh.host.call_result(&call.path, Outcome::Skipped, why, "");
        let Some(ch) = l.channel(&call.call.short_name, call.call.talkgroup) else { return skip("not a paging channel") };
        if !c.sends(ch) && !c.save_unknown {
            return skip("not sending this channel's pages to Rdio Scanner");
        }
        let (paged, unknown) = call_pages(&l, ch, &call.call.tone_patterns);
        let sends = c.sends(ch) && upload(&l, ch, &paged, unknown).is_some();
        let keeps = c.save_unknown && !unknown_patterns(&l, ch, &call.call.tone_patterns).is_empty();
        let wants = sends || keeps;
        let window = c.follow_on_s as i64 * 1000;
        if window == 0 {
            return if wants { finish(&self.sh, &self.queue, call) } else { skip("no pages") };
        }
        let take = self.joiner.lock().unwrap().concluded(ch, &call, wants, window);
        match take {
            join::Take::Joined => {}
            join::Take::Waits { flush } => {
                if flush {
                    // A new page: the one before goes up with what it has.
                    let w = self.joiner.lock().unwrap().waiting.remove(&ch);
                    if let Some(w) = w {
                        let page = join::ready(w, &self.sh.data_dir.join("joined"), &self.sh.host);
                        finish(&self.sh, &self.queue, page);
                    }
                }
                self.joiner.lock().unwrap().wait(ch, call);
            }
            join::Take::Skip => skip("no pages"),
        }
    }

    fn action(&mut self, a: Action, reply: Reply) {
        match a.action.as_str() {
            "fillAll" | "fillTimings" => {
                let one: Option<Agency> = (a.action == "fillTimings").then(|| serde_json::from_value(a.value.clone()).ok()).flatten();
                let sh = self.sh.clone();
                std::thread::spawn(move || match fill_timings(&sh, one.as_ref()) {
                    Ok(m) => reply.ok(m),
                    Err(e) => reply.fail(e),
                });
            }
            "testVoice" => {
                let c: Config = serde_json::from_value(a.value).unwrap_or_else(|_| self.sh.lists().cfg.clone());
                self.speaker.say(Say::Words { text: "Page alerts voice test.".into(), command: c.voice_command });
                reply.ok("Speaking now, on this computer's speakers");
            }
            "addCsv" => {
                let c: Config = serde_json::from_value(a.value).unwrap_or_else(|_| self.sh.lists().cfg.clone());
                match add_csv(&c) {
                    Ok((agencies, added, kept, note)) => {
                        self.sh.host.save_settings(&serde_json::json!({ "agencies": agencies }));
                        let what = format!("Added {added} tone sets from {} ({kept} already here, left as they were){note}", c.import_from);
                        self.sh.host.info(what.clone());
                        reply.ok(what);
                    }
                    Err(e) => reply.fail(e),
                }
            }
            "import" => {
                let c: Config = serde_json::from_value(a.value).unwrap_or_else(|_| self.sh.lists().cfg.clone());
                match import(&c.import_from) {
                    Ok(im) => {
                        let (nc, na, nm) = (im.channels.len(), im.agencies.len(), im.multi.len());
                        let mut s = serde_json::json!({ "agencies": im.agencies });
                        if !im.channels.is_empty() {
                            s["channels"] = serde_json::to_value(&im.channels).unwrap_or_default();
                        }
                        if !im.multi.is_empty() {
                            s["multiStation"] = serde_json::to_value(&im.multi).unwrap_or_default();
                        }
                        if let Some(v) = im.own_talkgroup {
                            s["ownTalkgroup"] = v.into();
                        }
                        self.sh.host.save_settings(&s);
                        let what = format!("{na} agencies, {nc} channels and {nm} Multi-Station rows{}", im.note);
                        self.sh.host.info(format!("Imported {what} from {}", c.import_from));
                        reply.ok(format!("Imported {what}"));
                    }
                    Err(e) => reply.fail(e),
                }
            }
            "testMulti" => {
                let m: MultiStation = match serde_json::from_value(a.value) {
                    Ok(x) => x,
                    Err(e) => return reply.fail(format!("Couldn't read that row: {e}")),
                };
                let sh = self.sh.clone();
                std::thread::spawn(move || match test_multi(&sh, &m) {
                    Ok(m) => reply.ok(m),
                    Err(e) => reply.fail(e),
                });
            }
            "testAgency" => {
                let ag: Agency = match serde_json::from_value(a.value) {
                    Ok(x) => x,
                    Err(e) => return reply.fail(format!("Couldn't read that agency: {e}")),
                };
                let sh = self.sh.clone();
                std::thread::spawn(move || match test_agency(&sh, &ag) {
                    Ok(m) => reply.ok(m),
                    Err(e) => reply.fail(e),
                });
            }
            _ => reply.fail("No such button"),
        }
    }

    fn settings(&mut self, setup: Setup<Config, NoConfig>) -> Result<(), String> {
        let lists = Lists::new(setup.config);
        warn_orphans(&self.sh.host, &lists);
        *self.sh.lists.lock().unwrap() = Arc::new(lists);
        self.sh.status();
        Ok(())
    }

    fn shutdown(&mut self, grace: Duration) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        // Pages still waiting for their dispatch go with what they have.
        flush_due(&self.sh, &self.joiner, &self.queue, true);
        self.queue.lock().unwrap().shutdown(grace);
    }
}

/// Agencies on a channel not set up here (FDNY's, heard by another recorder) are never matched: say so once.
fn warn_orphans(host: &Host, l: &Lists) {
    if !l.orphans.is_empty() {
        let mut names = l.orphans.clone();
        names.dedup();
        host.info(format!("{} agencies are on channels not set up here, so they aren't matched: {}", l.orphans.len(), names.join(", ")));
    }
}

/// What Tone Board's setup brings: its channels, agencies, Multi-Station rows and Rdio Scanner choices.
#[derive(Default)]
struct Imported {
    channels: Vec<Channel>,
    agencies: Vec<Agency>,
    multi: Vec<MultiStation>,
    own_talkgroup: Option<bool>,
    note: String,
}

/// From Tone Board's folder (its saved settings and the agency CSV they name), or an agency CSV alone.
fn import(from: &str) -> Result<Imported, String> {
    let p = expand(from);
    let mut out = Imported::default();
    let csv = if p.is_dir() {
        let ron = std::fs::read_to_string(p.join("app.ron")).map_err(|e| format!("{}: {e}", p.join("app.ron").display()))?;
        let csv = toneboard::agencies_path(&ron).filter(|f| f.exists()).unwrap_or_else(|| p.join("agencies.csv"));
        out.channels = toneboard::channels(&ron);
        // Only channels that follow a TRP talkgroup can be heard here.
        out.channels.retain(|c| parse_follow(&c.follow).is_some());
        out.multi = toneboard::multi_stations(&ron);
        if out.multi.is_empty() {
            out.note.push_str(if ron.contains("multi_stations") { " (its Multi-Station rows couldn't be read: send Claude the multi_stations part of app.ron)" } else { " (no Multi-Station rows in its app.ron)" });
        }
        (out.own_talkgroup, _, _) = toneboard::rdio_choices(&ron);
        csv
    } else {
        p
    };
    let text = std::fs::read_to_string(&csv).map_err(|e| format!("{}: {e}", csv.display()))?;
    let (agencies, bad) = agencies::read_csv(&text);
    if agencies.is_empty() {
        return Err(format!("No agencies in {}", csv.display()));
    }
    out.agencies = agencies;
    if !bad.is_empty() {
        out.note.push_str(&format!(" ({} lines skipped: {})", bad.len(), bad.join("; ")));
    }
    // Rows naming an agency that isn't on the list never fire: say which.
    let names: HashSet<String> = out.agencies.iter().map(|a| a.name.trim().to_lowercase()).collect();
    let missing: Vec<String> = out.multi.iter().flat_map(|m| m.agencies.iter()).filter(|a| !names.contains(&a.trim().to_lowercase())).cloned().collect();
    if !missing.is_empty() {
        out.note.push_str(&format!(" (Multi-Station names not on the agency list: {})", missing.join(", ")));
    }
    Ok(out)
}

/// The tone sets of the CSV `cfg.import_from` names, added to `cfg`'s (a name already there: kept as it is).
/// All of them, how many were added and kept, and a note.
fn add_csv(cfg: &Config) -> Result<(Vec<Agency>, usize, usize, String), String> {
    let p = expand(&cfg.import_from);
    if !p.is_file() {
        return Err(format!("{} isn't a CSV file: put the county's CSV in Import from", p.display()));
    }
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let (new, bad) = agencies::read_csv(&text);
    if new.is_empty() {
        return Err(format!("No tone sets in {}", p.display()));
    }
    let mut all = cfg.agencies.clone();
    let (mut added, mut kept) = (0, 0);
    for a in new {
        if all.iter().any(|x| x.name.trim().eq_ignore_ascii_case(a.name.trim())) {
            kept += 1;
        } else {
            all.push(a);
            added += 1;
        }
    }
    // Their channels: those not set up here are named, as the list's would be.
    let lists = Lists::new(Config { agencies: all.clone(), ..cfg.clone() });
    let mut note = if bad.is_empty() { String::new() } else { format!("; {} lines skipped: {}", bad.len(), bad.join("; ")) };
    let new_orphans = lists.orphans.iter().filter(|n| !cfg.agencies.iter().any(|a| &a.name == *n)).count();
    if new_orphans > 0 {
        note.push_str(&format!("; {new_orphans} are on a channel not set up here: add it under Channels"));
    }
    Ok((all, added, kept, note))
}

/// Tone Board's agency test: spoken here, and sent to Rdio Scanner where the agency's pages go.
fn test_agency(sh: &Shared, a: &Agency) -> Result<String, String> {
    let l = sh.lists();
    let c = &l.cfg;
    let text = format!("This is a Trunk Recorder Pro alert test for {}. This is only a test.", a.name);
    let f = sh.data_dir.join("agency-test.wav");
    test_audio(&text, &f)?;
    let _ = speak::play_file(&f);
    if c.rdio_server.trim().is_empty() || c.rdio_api_key.trim().is_empty() {
        return Ok("Spoken here; not sent: Rdio Scanner's server and API key aren't set".into());
    }
    // Where its pages go: its own talkgroup, or its channel's, on its channel's system (or the main one).
    let ch = l.cfg.agencies.iter().position(|x| x.name == a.name).and_then(|i| (0..c.channels.len()).find(|&k| l.agency_on(i, k)));
    let system = ch.map_or(c.rdio_system, |k| c.system_of(k));
    if system == 0 {
        return Ok("Spoken here; not sent: no Rdio Scanner system (its channel's, or the main one)".into());
    }
    let u = match (c.own_talkgroup, a.unit, ch.and_then(|k| c.channels[k].talkgroup)) {
        (true, Some(unit), _) => rdio::Upload { system, talkgroup: unit, label: a.name.clone(), source: Some(unit), patches: vec![] },
        (_, _, Some(tg)) => rdio::Upload { system, talkgroup: tg, label: ch.map(|k| c.channels[k].name.clone()).unwrap_or_default(), source: a.unit, patches: vec![] },
        _ => return Ok("Spoken here; not sent: it has no Unit ID and its channel no Rdio Scanner talkgroup".into()),
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    match rdio::send(&c.rdio_server, &c.rdio_api_key, &u, &f, now, 0) {
        Ok(()) => {
            sh.host.info(format!("Test for {} sent to Rdio Scanner: system {}, talkgroup {}", a.name, u.system, u.talkgroup));
            Ok(format!("Sent to Rdio Scanner: system {}, talkgroup {}", u.system, u.talkgroup))
        }
        Err(rdio::Failed::Again(e) | rdio::Failed::Refused(e)) => Err(e),
    }
}

/// Fills empty timings (of every tone set, or `one`) from the calls TRP has saved, and saves them.
fn fill_timings(sh: &Shared, one: Option<&Agency>) -> Result<String, String> {
    let l = sh.lists();
    if !sh.capture_dir.is_dir() {
        return Err(format!("TRP's recordings folder isn't there: {}", sh.capture_dir.display()));
    }
    let (heard, calls) = timings::heard(&l, &sh.capture_dir, 120);
    let mut agencies = l.cfg.agencies.clone();
    let mut done = Vec::new();
    let mut unheard = 0;
    for (i, a) in agencies.iter_mut().enumerate() {
        if let Some(o) = one {
            if a.name != o.name {
                continue;
            }
            // As edited on the form, but not saved yet.
            *a = o.clone();
        }
        match heard.get(&i) {
            Some(p) => {
                let what = timings::fill(a, p);
                if !what.is_empty() {
                    done.push(format!("{}: {what} ({} heard)", a.name, p.len()));
                }
            }
            None => unheard += 1,
        }
    }
    if one.is_some() && !l.cfg.agencies.iter().any(|a| Some(&a.name) == one.map(|o| &o.name)) {
        return Err("Save it first, then Fill timings".into());
    }
    if done.is_empty() {
        return Ok(match one {
            Some(_) if unheard > 0 => format!("No pages of it in the last 120 days of recordings ({calls} paging-channel calls looked at)"),
            Some(_) => "Nothing to fill: its timings are set, or its pages vary too much to pin down".into(),
            None => format!("Nothing to fill ({calls} paging-channel calls looked at; {unheard} tone sets not heard)"),
        });
    }
    sh.host.save_settings(&serde_json::json!({ "agencies": agencies }));
    for d in &done {
        sh.host.info(format!("Timings filled: {d}"));
    }
    Ok(match one {
        Some(_) => format!("Filled {}", done[0]),
        None => format!("Filled timings for {} tone sets ({unheard} not heard yet; details in the log)", done.len()),
    })
}

/// A test's audio: the words spoken, or (no voice here) three beeps.
fn test_audio(text: &str, f: &std::path::Path) -> Result<(), String> {
    if speak::words_to_wav(text, f).is_ok() {
        return Ok(());
    }
    let mut v = Vec::new();
    for _ in 0..3 {
        v.extend(speak::tone(1000.0, 0.4, 0.5));
        v.extend(std::iter::repeat_n(0i16, 3200));
    }
    speak::write_wav(f, &v, 8000).map_err(|e| e.to_string())
}

/// A Multi-Station row's test: one call on its talkgroup, patched with its tone sets', as a page of them goes.
fn test_multi(sh: &Shared, m: &MultiStation) -> Result<String, String> {
    let l = sh.lists();
    let c = &l.cfg;
    if c.rdio_server.trim().is_empty() || c.rdio_api_key.trim().is_empty() {
        return Err("Rdio Scanner's server and API key aren't set".into());
    }
    if m.talkgroup == 0 {
        return Err("It has no talkgroup".into());
    }
    let mut patches = Vec::new();
    let mut missing = Vec::new();
    for n in &m.agencies {
        match c.agencies.iter().find(|a| a.name.trim().eq_ignore_ascii_case(n.trim())) {
            Some(a) => patches.extend(a.unit.filter(|u| *u != m.talkgroup && !patches.contains(u))),
            None => missing.push(n.clone()),
        }
    }
    let text = format!("This is a Trunk Recorder Pro stacked page test for {}. This is only a test.", m.name);
    let f = sh.data_dir.join("multi-test.wav");
    test_audio(&text, &f)?;
    // Its system: its tone sets' channel's.
    let system = m.agencies.iter().filter_map(|n| c.agencies.iter().position(|a| a.name.trim().eq_ignore_ascii_case(n.trim()))).find_map(|i| (0..c.channels.len()).find(|&k| l.agency_on(i, k))).map_or(c.rdio_system, |k| c.system_of(k));
    if system == 0 {
        return Err("No Rdio Scanner system (its tone sets' channel's, or the main one)".into());
    }
    let u = rdio::Upload { system, talkgroup: m.talkgroup, label: m.name.clone(), source: None, patches: patches.clone() };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    rdio::send(&c.rdio_server, &c.rdio_api_key, &u, &f, now, 0).map_err(|(rdio::Failed::Again(e) | rdio::Failed::Refused(e))| e)?;
    let p = patches.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(", ");
    let warn = if missing.is_empty() { String::new() } else { format!(" ({} not on the tone set list)", missing.join(", ")) };
    sh.host.info(format!("Stacked page test for {} sent once: system {system}, talkgroup {}, patched with {p}{warn}", m.name, m.talkgroup));
    Ok(format!("Sent once: talkgroup {}, patched with {p}{warn}", m.talkgroup))
}

/// The queue's work: the one upload a concluded call's pages call for.
fn upload_call(sh: &Shared, call: &ConcludedCall) -> Attempt {
    let l = sh.lists();
    let c = &l.cfg;
    let Some(ch) = l.channel(&call.call.short_name, call.call.talkgroup) else { return Attempt::Skip("not a paging channel".into()) };
    let (paged, unknown) = call_pages(&l, ch, &call.call.tone_patterns);
    let Some(u) = upload(&l, ch, &paged, unknown) else { return Attempt::Skip("no pages to send".into()) };
    let audio = call.files.m4a.clone().filter(|p| p.exists()).unwrap_or_else(|| call.files.wav.clone());
    match rdio::send(&c.rdio_server, &c.rdio_api_key, &u, &audio, call.call.start_time, call.call.freq) {
        Ok(()) => {
            let also = if u.patches.is_empty() { String::new() } else { format!(", patched with {}", u.patches.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(", ")) };
            sh.host.info(format!("Sent to Rdio Scanner once: {} (TG {}{also})", u.label, u.talkgroup));
        }
        Err(rdio::Failed::Again(e)) => return Attempt::Retry(e),
        Err(rdio::Failed::Refused(e)) => return Attempt::Fail(e),
    }
    // A joined recording is done with.
    if audio.starts_with(sh.data_dir.join("joined")) {
        let _ = std::fs::remove_file(&audio);
    }
    Attempt::Done { url: String::new() }
}

fn main() {
    trunk_recorder_plugin::run::<PageAlerts>();
}

#[cfg(test)]
mod tests {
    use super::*;
    use trunk_recorder_plugin::{HostMessage, testing};

    fn two(a: f64, b: f64) -> TonePattern {
        TonePattern { pattern: "two_tone".into(), tones: vec![a, b], lengths: vec![1.0, 3.0], len: 4.0, ..Default::default() }
    }

    fn lists() -> Lists {
        let (agencies, _) = agencies::read_csv(
            "Empress EMS,1180,2575,,yes,beep,453.9,202,,,,\n\
Dutchess County EMS,1180,950,,yes,beep,453.9,201,,,,\n\
LaGrange FD EMS,1130,1232,,,,453.9,471,,,,\n\
Poughkeepsie FD 1st Alarm/EMS,1901,643,,yes,beep,453.65,571,,,,\n\
No unit,700,900,,,,453.9,,,,,\n",
        );
        Lists::new(Config {
            channels: vec![
                Channel { name: "DC 911".into(), freq_mhz: 453.9, follow: "UHF/1".into(), talkgroup: Some(1), rdio_system: None, send: false },
                Channel { name: "Poughkeepsie FD Dispatch".into(), freq_mhz: 453.65, follow: "UHF/31".into(), talkgroup: None, rdio_system: None, send: false },
            ],
            agencies,
            multi_station: vec![MultiStation { name: "EMS both".into(), agencies: vec!["Empress EMS".into(), "Dutchess County EMS".into()], talkgroup: 900, alert: true, sound: "beep".into() }],
            rdio_system: 5,
            ..Config::default()
        })
    }

    #[test]
    fn channels_and_agencies() {
        let l = lists();
        assert_eq!(l.channel("uhf", 1), Some(0));
        assert_eq!(l.channel("UHF", 31), Some(1));
        assert_eq!(l.channel("VHF", 1), None);
        let mut b = HashMap::new();
        assert_eq!(l.agency(0, &two(1179.98, 2574.93), &mut b), Some(0));
        assert_eq!(l.agency(0, &two(1179.98, 949.42), &mut b), Some(1));
        assert_eq!(l.agency(1, &two(1179.98, 949.42), &mut b), None, "on its own channel only");
        assert_eq!(l.agency(1, &two(1900.9, 643.1), &mut b), Some(3));
    }

    #[test]
    fn where_a_call_goes() {
        let l = lists();
        // Two tone sets of the Multi-Station row, and unknown tones, in one call.
        let (paged, unknown) = call_pages(&l, 0, &[two(1180.0, 2575.0), two(1180.0, 950.0), two(1500.0, 1600.0)]);
        assert_eq!((paged.as_slice(), unknown), (&[0, 1][..], true));
        let u = upload(&l, 0, &paged, unknown).unwrap();
        // Once: on the row's talkgroup, patched with each tone set's and DC 911's (for the unknown tones).
        assert_eq!((u.system, u.talkgroup, u.patches.clone(), u.label.as_str(), u.source), (5, 900, vec![202, 201, 1], "EMS both", Some(202)));
        // One tone set: its own talkgroup, nothing patched.
        let u = upload(&l, 0, &[0], false).unwrap();
        assert_eq!((u.talkgroup, u.patches.clone(), u.label.as_str()), (202, vec![], "Empress EMS"));
        // Two not in a row: the first's, the second patched, both named.
        let u = upload(&l, 0, &[0, 2], false).unwrap();
        assert_eq!((u.talkgroup, u.patches.clone(), u.label.as_str()), (202, vec![471], "Empress EMS + LaGrange FD EMS"));
        // A tone set without a Unit ID: on DC 911's.
        let u = upload(&l, 0, &[4], false).unwrap();
        assert_eq!((u.talkgroup, u.patches.clone()), (1, vec![]));
        // Each tone set not its own talkgroup: the channel's, the row's still main.
        let mut cfg = l.cfg.clone();
        cfg.own_talkgroup = false;
        let l2 = Lists::new(cfg);
        let u = upload(&l2, 0, &[0, 1], false).unwrap();
        assert_eq!((u.talkgroup, u.patches.clone()), (900, vec![1]));
        // Poughkeepsie has no channel talkgroup: an unknown page there goes nowhere.
        assert!(upload(&l, 1, &[], true).is_none());
    }

    #[test]
    fn imports_and_starts() {
        let dir = testing::temp_dir("page-alerts");
        std::fs::write(dir.join("app.ron"), r#"(settings:"(radios:[(freq_hz:453900000.0,channel_name:\"DC 911\",talkgroup:\"\",follow_talkgroup:\"UHF/1\",relay_talkgroup:\"\",extra_channels:[(freq_hz:482231000.0,name:\"FDNY\",talkgroup:\"201\",follow_talkgroup:\"https://openmhz.com/x\")])])")"#).unwrap();
        std::fs::write(dir.join("agencies.csv"), "name,tone_a,tone_b\nEmpress EMS,1180,2575,,yes,beep,453.9,202,,,,\n").unwrap();
        let im = import(dir.to_str().unwrap()).unwrap();
        assert_eq!((im.channels.len(), im.channels[0].follow.as_str(), im.agencies.len()), (1, "UHF/1", 1), "the OpenMHz channel isn't TRP's");
        let hello = testing::hello(&dir, serde_json::json!({}));
        let out = testing::run::<PageAlerts>([HostMessage::Hello(hello)]);
        assert!(out.ready());
    }

    #[test]
    fn its_form() {
        let m = trunk_recorder_plugin::describe::<PageAlerts>();
        let c = m.config.unwrap();
        assert_eq!(c["x-actions"][0]["id"], "fillAll");
        assert!(c["properties"].get("duty").is_none());
        let ag = &c["properties"]["agencies"]["items"];
        assert_eq!((ag["title"].as_str(), ag["x-title"].as_str()), (Some("Tone set"), Some("name")), "{ag:#}");
        assert_eq!(ag["properties"]["alert"]["type"], "boolean");
        // Saved by 0.1.x: an Alert choice, and duty.
        let old: Config = serde_json::from_value(serde_json::json!({ "duty": "offDuty", "agencies": [{ "name": "A", "toneA": 950.0, "alert": "onDuty" }, { "name": "B", "toneA": 950.0, "alert": "off" }], "multiStation": [{ "name": "M", "alert": "both" }] })).unwrap();
        assert_eq!((old.agencies[0].alert, old.agencies[1].alert, old.multi_station[0].alert), (true, false, true));
        assert_eq!(c["properties"]["rdioApiKey"]["x-secret"], true);
        assert!(m.subscribe.contains(&"tone_pattern".to_string()));
    }
}

#[cfg(test)]
mod real_list {
    use super::*;

    /// Bill's list against what TRP heard on DC911 (the Tones card, 2026-11-23).
    #[test]
    fn names_what_trp_heard() {
        let Ok(text) = std::fs::read_to_string("/root/.claude/uploads/96d8d546-18b5-580b-af36-5611a9448b22/f34618a5-agencies.csv") else { return };
        let (agencies, bad) = agencies::read_csv(&text);
        assert!(bad.is_empty(), "{bad:?}");
        let l = Lists::new(Config { channels: vec![Channel { name: "DC 911".into(), freq_mhz: 453.9, follow: "UHF/1".into(), talkgroup: None, rdio_system: None, send: false }], agencies, ..Config::default() });
        let heard = [
            (1179.98, 2574.93, 2.0, 1.0),
            (1820.0, 2259.96, 2.0, 1.02),
            (1130.06, 1231.97, 2.0, 1.0),
            (1179.98, 949.42, 1.0, 3.0),
            (731.93, 1285.88, 2.0, 0.98),
            (1036.09, 701.12, 2.0, 1.0),
            (2574.91, 799.09, 3.0, 0.5),
        ];
        let mut names = Vec::new();
        for (a, b, la, lb) in heard {
            let p = TonePattern { pattern: "two_tone".into(), tones: vec![a, b], lengths: vec![la, lb], len: la + lb, ..Default::default() };
            names.push(l.agency(0, &p, &mut HashMap::new()).map(|i| l.cfg.agencies[i].name.clone()).unwrap_or_default());
        }
        eprintln!("{names:#?}");
        assert!(names.iter().all(|n| !n.is_empty()));
    }
}

#[cfg(test)]
mod end_to_end {
    use super::*;
    use trunk_recorder_plugin::{HostMessage, testing};

    /// A page heard on DC 911: logged as the agency's, then its call goes to Rdio Scanner under the agency's talkgroup.
    #[test]
    fn a_page_is_named_and_sent() {
        let dir = testing::temp_dir("page-alerts-e2e");
        let server = testing::MockServer::start(|_| (200, "Call imported successfully.".into()));
        let cfg = serde_json::json!({
            "channels": [{ "name": "DC 911", "freqMhz": 453.9, "follow": "UHF/1" }],
            "agencies": [{ "name": "Empress EMS", "toneA": 1180.0, "toneB": 2575.0, "channel": "453.9", "alert": "off", "unit": 202 }],
            "sendPages": true, "rdioServer": server.url(), "rdioApiKey": "k", "rdioSystem": 7, "speak": false,
        });
        let hello = testing::hello(&dir, cfg);
        let p = TonePattern { pattern: "two_tone".into(), tones: vec![1179.98, 2574.93], lengths: vec![1.0, 3.0], len: 4.0, text: "two-tone 1179.98 → 2574.93 Hz".into(), ..Default::default() };
        let ev = TonePatternEvent { short_name: "UHF".into(), talkgroup: 1, call_num: 1, pattern: p.clone(), ..Default::default() };
        let mut call = testing::call(&dir, "UHF", 1);
        call.call.tone_patterns = vec![p];
        let out = testing::run::<PageAlerts>([HostMessage::Hello(hello), HostMessage::TonePattern(ev), HostMessage::CallConcluded(call)]);
        let logs: Vec<String> = out.logs().into_iter().map(|(_, t)| t).collect();
        assert!(logs.iter().any(|l| l.starts_with("PAGE DC 911: Empress EMS")), "{logs:#?}");
        let reqs = server.requests();
        assert_eq!(reqs.len(), 1, "{logs:#?}");
        let f = |k: &str| String::from_utf8(reqs[0].form_field(k).unwrap_or_default()).unwrap();
        assert_eq!((f("system"), f("talkgroup"), f("source"), f("talkgroupLabel"), f("key")), ("7".into(), "202".into(), "202".into(), "Empress EMS".into(), "k".into()));
        assert_eq!(reqs[0].path, "/api/call-upload");
    }

    /// Tones, the carrier drops, and the dispatcher keys up 10 s later: one upload, both calls' audio in it.
    #[test]
    fn the_dispatch_after_is_joined() {
        let dir = testing::temp_dir("page-alerts-join");
        let server = testing::MockServer::start(|_| (200, "Call imported successfully.".into()));
        let cfg = serde_json::json!({
            "channels": [{ "name": "DC 911", "freqMhz": 453.9, "follow": "UHF/1" }],
            "agencies": [{ "name": "Empress EMS", "toneA": 1180.0, "toneB": 2575.0, "channel": "453.9", "alert": false, "unit": 202 }],
            "sendPages": true, "rdioServer": server.url(), "rdioApiKey": "k", "rdioSystem": 7, "speak": false, "followOnS": 15,
        });
        let hello = testing::hello(&dir, cfg);
        let p = TonePattern { pattern: "two_tone".into(), tones: vec![1179.98, 2574.93], lengths: vec![1.0, 3.0], len: 4.0, ..Default::default() };
        let now = join::now_ms();
        let mut page = testing::call(&dir, "UHF", 1);
        page.call.call_num = 1;
        page.call.tone_patterns = vec![p];
        page.call.start_time_ms = now - 15_000;
        page.call.stop_time_ms = now - 11_000;
        let _ = std::fs::remove_file(page.files.m4a.take().unwrap());
        let mut after = page.clone();
        after.path = format!("{}-b", page.path);
        after.call.call_num = 2;
        after.call.tone_patterns = vec![];
        after.call.start_time_ms = now - 1_000;
        after.call.stop_time_ms = now;
        after.files.wav = dir.join("after.wav");
        speak::write_wav(&after.files.wav, &vec![500i16; 8000 * 5], 8000).unwrap();
        let start = CallInfo { id: 2, short_name: "UHF".into(), talkgroup: 1, ..Default::default() };
        let other = { let mut o = page.clone(); o.path = "VHF/x".into(); o.call.short_name = "VHF".into(); o };
        let out = testing::run::<PageAlerts>([HostMessage::Hello(hello), HostMessage::CallConcluded(page.clone()), HostMessage::CallStart(start), HostMessage::CallConcluded(after.clone()), HostMessage::CallConcluded(other)]);
        let logs: Vec<String> = out.logs().into_iter().map(|(_, t)| t).collect();
        let reqs = server.requests();
        assert_eq!(reqs.len(), 1, "{logs:#?}");
        let audio = reqs[0].form_field("audio").unwrap_or_default();
        // 3 s + 0.5 s + 5 s of 8 kHz audio: well past the page's own 3 s.
        assert!(audio.len() > 8000 * 2 * 6 || String::from_utf8(reqs[0].form_field("audioType").unwrap_or_default()).unwrap() == "audio/mp4", "{} bytes; {logs:#?}", audio.len());
        assert!(logs.iter().any(|l| l.starts_with("Joined 1 call")), "{logs:#?}");
        let r = out.results();
        assert!(r.iter().any(|(p, o, m, _)| p == &after.path && *o == Outcome::Skipped && m.starts_with("joined")), "{r:#?}");
        assert!(r.iter().any(|(p, o, _, _)| p == "VHF/x" && *o == Outcome::Skipped), "every call is reported on: {r:#?}");
        assert!(r.iter().any(|(p, o, _, _)| p == &page.path && *o == Outcome::Ok), "{r:#?}");
    }

    /// Two tone sets in two transmissions, then the dispatch: one upload, on the Multi-Station row's talkgroup, the tone sets' patched.
    #[test]
    fn a_stacked_page_goes_up_once() {
        let dir = testing::temp_dir("page-alerts-stack");
        let server = testing::MockServer::start(|_| (200, "Call imported successfully.".into()));
        let cfg = serde_json::json!({
            "channels": [{ "name": "DC 911", "freqMhz": 453.9, "follow": "UHF/1", "talkgroup": 1 }],
            "agencies": [
                { "name": "Empress EMS", "toneA": 1180.0, "toneB": 2575.0, "channel": "453.9", "alert": false, "unit": 202 },
                { "name": "Dutchess County EMS", "toneA": 1180.0, "toneB": 950.0, "channel": "453.9", "alert": false, "unit": 201 }
            ],
            "multiStation": [{ "name": "EMS both", "agencies": ["Empress EMS", "Dutchess County EMS"], "talkgroup": 900, "alert": false }],
            "sendPages": true, "rdioServer": server.url(), "rdioApiKey": "k", "rdioSystem": 7, "speak": false, "followOnS": 15,
        });
        let hello = testing::hello(&dir, cfg);
        let now = join::now_ms();
        let mk = |n: u64, tones: Option<(f64, f64)>, start: i64, secs: i64| {
            let mut c = testing::call(&dir, "UHF", 1);
            c.path = format!("UHF/2026/11/24/1-{n}");
            c.call.call_num = n;
            c.call.start_time_ms = now + start;
            c.call.stop_time_ms = now + start + secs * 1000;
            c.call.tone_patterns = tones.map(|(a, b)| vec![TonePattern { pattern: "two_tone".into(), tones: vec![a, b], lengths: vec![1.0, 3.0], len: 4.0, ..Default::default() }]).unwrap_or_default();
            c.files.m4a = None;
            c.files.wav = dir.join(format!("{n}.wav"));
            speak::write_wav(&c.files.wav, &vec![300i16; 8000 * secs as usize], 8000).unwrap();
            c
        };
        let (a, b, v) = (mk(1, Some((1180.0, 2575.0)), -30_000, 4), mk(2, Some((1180.0, 950.0)), -24_000, 4), mk(3, None, -12_000, 8));
        let out = testing::run::<PageAlerts>([HostMessage::Hello(hello), HostMessage::CallConcluded(a), HostMessage::CallConcluded(b), HostMessage::CallConcluded(v)]);
        let logs: Vec<String> = out.logs().into_iter().map(|(_, t)| t).collect();
        let reqs = server.requests();
        assert_eq!(reqs.len(), 1, "{logs:#?}");
        let f = |k: &str| String::from_utf8(reqs[0].form_field(k).unwrap_or_default()).unwrap();
        assert_eq!((f("system"), f("talkgroup"), f("patches"), f("talkgroupLabel")), ("7".into(), "900".into(), "[202,201]".into(), "EMS both".into()), "{logs:#?}");
        assert!(logs.iter().any(|l| l.starts_with("Joined 2 call")), "{logs:#?}");
    }

    /// Pages aren't sent until Send pages is on, but a Multi-Station row's Test is: once, patched with its tone sets'.
    #[test]
    fn tests_before_the_cut_over() {
        let dir = testing::temp_dir("page-alerts-cutover");
        let server = testing::MockServer::start(|_| (200, "Call imported successfully.".into()));
        let cfg: Config = serde_json::from_value(serde_json::json!({
            "channels": [{ "name": "DC 911", "freqMhz": 453.9, "follow": "UHF/1", "talkgroup": 1 }],
            "agencies": [
                { "name": "Chelsea FD All", "toneA": 1180.0, "toneB": 2575.0, "unit": 35 },
                { "name": "FDVA Castle Point", "toneA": 1180.0, "toneB": 950.0, "unit": 36 }
            ],
            "rdioServer": server.url(), "rdioApiKey": "k", "rdioSystem": 7, "speak": false,
        })).unwrap();
        assert!(!cfg.send_pages, "off until the cut-over");
        let (host, _seen) = testing::capture();
        let sh = Shared { host, data_dir: dir.clone(), capture_dir: dir.clone(), lists: Mutex::new(Arc::new(Lists::new(cfg.clone()))), last: Mutex::new(String::new()) };
        let row = MultiStation { name: "Chelsea + FDVA CP".into(), agencies: vec!["Chelsea FD All".into(), "FDVA Castle Point".into()], talkgroup: 1413591, ..Default::default() };
        let m = test_multi(&sh, &row).unwrap();
        assert!(m.contains("patched with 35, 36"), "{m}");
        let reqs = server.requests();
        let f = |k: &str| String::from_utf8(reqs[0].form_field(k).unwrap_or_default()).unwrap();
        assert_eq!((reqs.len(), f("talkgroup"), f("patches")), (1, "1413591".into(), "[35,36]".into()));
        // A real page isn't sent.
        let hello = testing::hello(&dir, serde_json::to_value(&cfg).unwrap());
        let mut page = testing::call(&dir, "UHF", 1);
        page.call.tone_patterns = vec![TonePattern { pattern: "two_tone".into(), tones: vec![1180.0, 2575.0], lengths: vec![1.0, 3.0], len: 4.0, ..Default::default() }];
        let out = testing::run::<PageAlerts>([HostMessage::Hello(hello), HostMessage::CallConcluded(page.clone())]);
        assert_eq!(server.requests().len(), 1);
        assert!(out.results().iter().any(|(p, o, _, _)| p == &page.path && *o == Outcome::Skipped));
    }

    /// A county's tone sets added from its CSV, its channel on a Rdio Scanner system of its own.
    #[test]
    fn a_county_added_on_its_own_system() {
        let dir = testing::temp_dir("page-alerts-county");
        let csv = dir.join("ulster.csv");
        std::fs::write(&csv, "name,tone_a,tone_b,notes,alert,sound,channel,unit,a_s,gap_s,b_s,beeps\nMobile Life,399.2,1530,,yes,beep,155.220,1001,,,,\nEmpress EMS,1180,2575,,yes,beep,453.9,9,,,,\nKingston All-Call,1122.5,1433.4,,yes,beep,155.220,1002,,,,\n").unwrap();
        let cfg = Config {
            channels: vec![
                Channel { name: "DC 911".into(), freq_mhz: 453.9, follow: "UHF/1".into(), talkgroup: None, rdio_system: None, send: false },
                Channel { name: "Ulster".into(), freq_mhz: 155.22, follow: "VHF/105".into(), talkgroup: None, rdio_system: Some(150), send: true },
            ],
            agencies: agencies::read_csv("Empress EMS,1180,2575,,yes,beep,453.9,202,,,,\n").0,
            rdio_system: 5,
            import_from: csv.display().to_string(),
            ..Config::default()
        };
        let (all, added, kept, note) = add_csv(&cfg).unwrap();
        assert_eq!((all.len(), added, kept), (3, 2, 1), "{note}");
        assert_eq!(all[0].unit, Some(202), "the one here is left as it is");
        let l = Lists::new(Config { agencies: all, ..cfg });
        let two = |a: f64, b: f64| TonePattern { pattern: "two_tone".into(), tones: vec![a, b], lengths: vec![2.0, 3.0], len: 5.0, ..Default::default() };
        let (paged, unknown) = call_pages(&l, 1, &[two(399.7, 1530.0)]);
        let u = upload(&l, 1, &paged, unknown).unwrap();
        assert_eq!((u.system, u.talkgroup, u.label.as_str()), (150, 1001, "Mobile Life"));
        // DC 911's still go to the main system.
        let (paged, _) = call_pages(&l, 0, &[two(1180.0, 2575.0)]);
        assert_eq!(upload(&l, 0, &paged, false).unwrap().system, 5);
        // The real Ulster list reads whole.
        if let Ok(t) = std::fs::read_to_string("/mnt/user-data/outputs/ulster-fire-tones.csv") {
            let (a, bad) = agencies::read_csv(&t);
            assert!(bad.is_empty(), "{bad:?}");
            assert_eq!(a.len(), 139);
        }
    }

    /// Send pages off (Tone Board still sends DC 911's), Ulster's channel on: only Ulster's go, to its system.
    #[test]
    fn only_the_channels_switched_on_send() {
        let dir = testing::temp_dir("page-alerts-some");
        let server = testing::MockServer::start(|_| (200, "Call imported successfully.".into()));
        let cfg = serde_json::json!({
            "channels": [
                { "name": "DC 911", "freqMhz": 453.9, "follow": "UHF/1", "talkgroup": 1 },
                { "name": "Ulster", "freqMhz": 155.22, "follow": "VHF/105", "rdioSystem": 56001, "send": true }
            ],
            "agencies": [
                { "name": "Empress EMS", "toneA": 1180.0, "toneB": 2575.0, "channel": "453.9", "alert": false, "unit": 202 },
                { "name": "Mobile Life", "toneA": 399.2, "toneB": 1530.0, "channel": "155.220", "alert": false, "unit": 1001 }
            ],
            "rdioServer": server.url(), "rdioApiKey": "k", "rdioSystem": 7, "speak": false, "followOnS": 0,
        });
        let hello = testing::hello(&dir, cfg);
        let pat = |a: f64, b: f64| TonePattern { pattern: "two_tone".into(), tones: vec![a, b], lengths: vec![2.0, 3.0], len: 5.0, ..Default::default() };
        let mut dc = testing::call(&dir, "UHF", 1);
        dc.call.tone_patterns = vec![pat(1180.0, 2575.0)];
        let mut ul = testing::call(&dir, "VHF", 105);
        ul.call.tone_patterns = vec![pat(399.7, 1530.0)];
        let out = testing::run::<PageAlerts>([HostMessage::Hello(hello), HostMessage::CallConcluded(dc.clone()), HostMessage::CallConcluded(ul)]);
        let reqs = server.requests();
        assert_eq!(reqs.len(), 1, "{:#?}", out.logs());
        let f = |k: &str| String::from_utf8(reqs[0].form_field(k).unwrap_or_default()).unwrap();
        assert_eq!((f("system"), f("talkgroup"), f("talkgroupLabel")), ("56001".into(), "1001".into(), "Mobile Life".into()));
        assert!(out.results().iter().any(|(p, o, _, _)| p == &dc.path && *o == Outcome::Skipped), "DC 911's is Tone Board's");
        assert!(out.status().is_some_and(|(_, m)| m.contains("sending only Ulster")), "{:?}", out.status());
    }

    /// A tone set's Test goes to its channel's system when the main one isn't set.
    #[test]
    fn a_test_goes_to_its_channels_system() {
        let dir = testing::temp_dir("page-alerts-test-sys");
        let server = testing::MockServer::start(|_| (200, "Call imported successfully.".into()));
        let cfg: Config = serde_json::from_value(serde_json::json!({
            "channels": [{ "name": "Ulster", "freqMhz": 155.22, "follow": "VHF/105", "rdioSystem": 56001, "send": true }],
            "agencies": [{ "name": "Mobile Life", "toneA": 399.2, "toneB": 1530.0, "channel": "155.220", "unit": 1056 }],
            "rdioServer": server.url(), "rdioApiKey": "k", "speak": false,
        })).unwrap();
        assert_eq!(cfg.rdio_system, 0);
        let (host, _seen) = testing::capture();
        let sh = Shared { host, data_dir: dir.clone(), capture_dir: dir.clone(), lists: Mutex::new(Arc::new(Lists::new(cfg.clone()))), last: Mutex::new(String::new()) };
        let m = test_agency(&sh, &cfg.agencies[0]).unwrap();
        assert!(m.contains("system 56001, talkgroup 1056"), "{m}");
        let reqs = server.requests();
        let f = |k: &str| String::from_utf8(reqs[0].form_field(k).unwrap_or_default()).unwrap();
        assert_eq!((f("system"), f("talkgroup")), ("56001".into(), "1056".into()));
    }

    /// Unknown tones on a channel that isn't sent: the page and the dispatch after it are kept, with a note.
    #[test]
    fn unknown_pages_are_kept() {
        let dir = testing::temp_dir("page-alerts-keep");
        let keep = dir.join("unknown");
        let cfg = serde_json::json!({
            "channels": [{ "name": "Ulster", "freqMhz": 155.22, "follow": "VHF/105" }],
            "agencies": [{ "name": "Mobile Life", "toneA": 399.2, "toneB": 1530.0, "channel": "155.220", "unit": 1056 }],
            "speak": false, "followOnS": 15, "saveUnknown": true, "unknownDir": keep.display().to_string(),
        });
        let hello = testing::hello(&dir, cfg);
        let now = join::now_ms();
        let mk = |n: u64, tones: Vec<TonePattern>, start: i64, secs: i64| {
            let mut c = testing::call(&dir, "VHF", 105);
            c.path = format!("VHF/2026/11/25/105-{n}");
            c.call.call_num = n;
            c.call.start_time_ms = now + start;
            c.call.stop_time_ms = now + start + secs * 1000;
            c.call.tone_patterns = tones;
            c.files.m4a = None;
            c.files.wav = dir.join(format!("{n}.wav"));
            speak::write_wav(&c.files.wav, &vec![300i16; 8000 * secs as usize], 8000).unwrap();
            c
        };
        let pat = |a: f64, b: f64| TonePattern { pattern: "two_tone".into(), tones: vec![a, b], lengths: vec![2.0, 3.0], len: 5.0, text: format!("two-tone {a} → {b} Hz, 2 + 3 s"), ..Default::default() };
        let known = mk(1, vec![pat(399.7, 1530.0)], -60_000, 5);
        let page = mk(2, vec![pat(1820.0, 992.0)], -30_000, 5);
        let voice = mk(3, vec![], -20_000, 8);
        let out = testing::run::<PageAlerts>([HostMessage::Hello(hello), HostMessage::CallConcluded(known.clone()), HostMessage::CallConcluded(page.clone()), HostMessage::CallConcluded(voice.clone())]);
        let logs: Vec<String> = out.logs().into_iter().map(|(_, t)| t).collect();
        let kept: Vec<String> = logs.iter().filter(|l| l.starts_with("Unknown tones kept")).cloned().collect();
        assert_eq!(kept.len(), 1, "only the unknown page: {logs:#?}");
        assert!(kept[0].contains("Ulster 1820-992"), "{kept:?}");
        let index = std::fs::read_to_string(keep.join("index.csv")).unwrap();
        assert!(index.contains("1820.0 → 992.0"), "{index}");
        // Every call is reported on: none sent, the dispatch joined.
        let r = out.results();
        for c in [&known, &page, &voice] {
            assert!(r.iter().any(|(p, o, _, _)| p == &c.path && *o == Outcome::Skipped), "{}: {r:#?}", c.path);
        }
    }
}
