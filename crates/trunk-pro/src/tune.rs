//! Tuning a radio's gain while it records ([`trunk_app::tune`]): the
//! recorder's side, and `trunk-pro tune`, which asks a recorder for it.
//!
//! A tuning runs on a thread of its own. Each candidate the radio's plan
//! ([`GainPlan`]) proposes goes on the air as a trial ([`Ctx::trial_gain`]):
//! the radio takes it at once, and nothing is saved or told the browsers.
//! Each second's `stats` come to it from the session ([`Slot::stats`]), the
//! same values the dashboard has; the systems scored are those whose control
//! channels the engine runs on that radio. However it ends — done, `tuneStop`,
//! an error — the radio goes back to what the run has, unless `apply` keeps
//! the best, which is then saved like any edit. A recording that stops ends it
//! (the radio is closed; the config was never changed). Setup editing that
//! radio meanwhile stops it too: the edit wins.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use trunk_app::gain::{GainPlan, Point};
use trunk_app::protocol::{Phase, ToClient};
use trunk_app::tune::{self as tn, Cmp, Found, Step, Stop, TunePhase, TuneReport, TuneRequest, TuneTry, Values, Watch};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::config::Source;
use crate::runtime::{publish, Ctx, EngineCmd, Hub};
use crate::{die, Args};

/// The tuning going on, if one is, and its last report (with its radio's label).
#[derive(Default)]
pub struct Slot {
    active: Mutex<Option<Active>>,
    last: Mutex<Option<(String, TuneReport)>>,
}

struct Active {
    /// The radio, by its label ([`Source::label`]).
    label: String,
    ticks: mpsc::Sender<(f64, Values)>,
    /// Why it's to stop, once it is.
    stop: Arc<Mutex<Option<String>>>,
}

impl Slot {
    /// A second's `stats` values: to the tuning, if one is going on.
    pub fn stats(&self, t: f64, values: &Values) {
        if let Some(a) = self.active.lock().unwrap().as_ref() {
            let _ = a.ticks.send((t, values.clone()));
        }
    }

    /// Stop the tuning (`why`): the radio goes back as it was.
    pub fn stop(&self, why: &str) {
        if let Some(a) = self.active.lock().unwrap().as_ref() {
            a.stop.lock().unwrap().get_or_insert_with(|| why.to_string());
        }
    }

    /// The recording stopped: so does the tuning.
    pub fn recording_ended(&self) {
        if let Some(a) = self.active.lock().unwrap().take() {
            a.stop.lock().unwrap().get_or_insert_with(|| "recording stopped".into());
        }
    }

    /// Setup changed source `src`: a tuning of it stops (the edit wins); one
    /// done is outdated (what it found was about the gain it had), and the browsers are told.
    pub fn edited(&self, hub: &Hub, src: &Source) {
        let tuned = self.active.lock().unwrap().as_ref().is_some_and(|a| a.label == src.label());
        if tuned {
            self.stop("its settings were changed in Setup");
            return;
        }
        let mut last = self.last.lock().unwrap();
        if let Some((_, r)) = last.as_mut().filter(|(l, r)| *l == src.label() && !r.outdated) {
            r.outdated = true;
            publish(hub, ToClient::Tune(r.clone()));
        }
    }

    /// The last report, for a browser that connects.
    pub fn last(&self) -> Option<ToClient> {
        self.last.lock().unwrap().clone().map(|(_, r)| ToClient::Tune(r))
    }
}

/// What's fixed for a tuning: the radio, its systems, the timings.
struct Plan {
    /// The radio's place in the run.
    k: usize,
    plan: GainPlan,
    /// The radio as the run has it (what each candidate changes the gain of).
    base: Source,
    start: Point,
    /// The start written back isn't quite what the radio has (another
    /// Airspy mode, AGC on, a gain off its table): the start's slices are
    /// the radio as it is, so the first comparisons are against what it had.
    as_it_is: bool,
    label: String,
    watches: Vec<Watch>,
    names: Vec<String>,
    scored: Option<Vec<String>>,
    protected: Vec<String>,
    req: TuneRequest,
}

/// `tuneGain`: with `check`, the plan, to the asker; else the tuning, started.
pub fn start(ctx: &Arc<Ctx>, req: TuneRequest) -> Result<Option<ToClient>, String> {
    let req = req.clamped();
    if ctx.phase.lock().unwrap().phase != Phase::Running {
        return Err("Tuning a gain needs the recorder recording: it scores what the radio's systems decode. Start first.".into());
    }
    let cfg = ctx.config.lock().unwrap().clone();
    let i = req.source;
    let title = cfg.sources.get(i).map(|s| s.title(i)).ok_or_else(|| format!("There's no source {}.", i + 1))?;
    let (k, base) = {
        let run = ctx.run.lock().unwrap();
        let run = run.as_ref().ok_or("The recorder isn't recording.")?;
        let k = cfg.same_radio(i, &run.sources).ok_or_else(|| format!("{title} isn't recording (it was added since Start)."))?;
        (k, run.sources[k].clone())
    };
    // (The gains its radio said it has, where they aren't its kind's own: a SoapySDR radio's gain elements too.)
    let plan = GainPlan::of_with(&base, req.mode, &crate::radio::reported_gains(&base)).map_err(|e| format!("{title}: {e}"))?;
    let start = plan.point(&base);
    let as_it_is = plan.apply(&base, &start) != base;
    if crate::runtime::live_gain(&base, &plan.apply(&base, &start)).is_none() {
        return Err(format!("{title}: its gain can't be changed while recording."));
    }
    // The systems: those whose control channels the engine runs on this radio.
    let (tx, rx) = mpsc::channel();
    ctx.tell_engine(EngineCmd::Sites { done: tx });
    let sites = rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "The recording didn't answer.".to_string())?;
    let watches = tn::watches(&sites, k);
    if watches.is_empty() {
        return Err(format!("{title} receives no system's control channel now: there's nothing to score it by."));
    }
    let names: Vec<String> = watches.iter().map(|w| w.short_name.clone()).collect();
    let scored = match req.systems.as_slice() {
        [] => None,
        only => {
            if let Some(missing) = only.iter().find(|s| !names.contains(s)) {
                return Err(format!("{missing}'s control channel isn't on {title} (it has {}).", names.join(", ")));
            }
            Some(only.to_vec())
        }
    };
    let protected: Vec<String> = scored.as_ref().map_or_else(Vec::new, |s| names.iter().filter(|n| !s.contains(n)).cloned().collect());
    let tried = req.try_setting.as_ref().map(|s| plan.point_of(&start, s)).transpose().map_err(|e| format!("try: {e}"))?;
    if tried.as_ref() == Some(&start) && !as_it_is {
        return Err(format!("try: that's the setting {title} has ({}).", plan.setting(&start)));
    }
    let estimate_s = req.per_candidate_s() * if tried.is_some() { 1.0 } else { req.max_steps as f64 + 1.0 };
    let report = TuneReport {
        phase: if req.check { TunePhase::Planned } else { TunePhase::Running },
        source: i,
        radio: title,
        plan: plan.about.clone(),
        scored: scored.clone().unwrap_or_else(|| names.clone()),
        watched: protected.clone(),
        start: plan.setting(&start),
        estimate_s,
        ..Default::default()
    };
    if req.check {
        return Ok(Some(ToClient::Tune(report)));
    }
    let stop = Arc::new(Mutex::new(None));
    let (ticks, rx) = mpsc::channel();
    {
        let mut active = ctx.tune.active.lock().unwrap();
        if active.is_some() {
            return Err("A gain is already being tuned: stop it first.".into());
        }
        *active = Some(Active { label: base.label(), ticks, stop: stop.clone() });
    }
    let p = Plan { k, plan, label: base.label(), base, start, as_it_is, watches, names, scored, protected, req };
    let ctx2 = ctx.clone();
    std::thread::Builder::new().name("tune".into()).spawn(move || run(&ctx2, p, tried, report, &rx, &stop)).map_err(|e| e.to_string())?;
    Ok(None)
}

/// Tell the browsers how it's going.
fn tell(ctx: &Ctx, label: &str, r: &TuneReport) {
    *ctx.tune.last.lock().unwrap() = Some((label.to_string(), r.clone()));
    publish(&ctx.hub, ToClient::Tune(r.clone()));
}

fn run(ctx: &Ctx, p: Plan, tried: Option<Point>, mut report: TuneReport, rx: &mpsc::Receiver<(f64, Values)>, stop: &Mutex<Option<String>>) {
    log::info!("{}: tuning the gain from {} ({})", report.radio, report.start, p.plan.about.join("; "));
    let began = Instant::now();
    tell(ctx, &p.label, &report);
    let mut feed = Feed { ctx, p: &p, rx, stop, last: None };
    let mut compare = |inc: &Point, cand: &Point, final_check: bool, report: &mut TuneReport| -> Result<Option<Cmp>, Stop> {
        report.now = Some(p.plan.setting(cand));
        tell(ctx, &p.label, report);
        let (cmp, inc_step, cand_step) = feed.compare(inc, cand).map_err(Stop::Interrupted)?;
        report.tried.push(TuneTry {
            candidate: p.plan.setting(cand),
            against: p.plan.setting(inc),
            cmp,
            kept: !final_check && cmp.is_some_and(|c| c.better(p.req.margin)),
            why: cand_step.abandoned.clone().or_else(|| cand_step.clipping().then(|| format!("clipping {:.2}%", cand_step.src.clip.get().unwrap_or(0.0)))),
            systems: tn::figures(&inc_step, &cand_step, &p.names, p.scored.as_deref()),
            clip_pct: cand_step.src.clip.get().unwrap_or(0.0),
            final_check,
        });
        (report.now, report.elapsed_s) = (None, began.elapsed().as_secs_f64());
        tell(ctx, &p.label, report);
        Ok(cmp)
    };
    let result = match &tried {
        Some(t) => compare(&p.start, t, false, &mut report).map(|c| (Found { best: t.clone(), ..Default::default() }, c)),
        None => {
            let found = {
                let r = &mut report;
                tn::search(&p.plan, p.start.clone(), p.req.margin, p.req.max_steps, &mut |inc, cand| compare(inc, cand, false, r))
            };
            // The search moved a step at a time; the best against the start, once more, decides.
            found.and_then(|found| match found.best == p.start {
                true => Ok((found, None)),
                false => compare(&p.start, &found.best, true, &mut report).map(|c| (found, c)),
            })
        }
    };
    report.elapsed_s = began.elapsed().as_secs_f64();
    match result {
        Ok((found, check)) => {
            let better = check.is_some_and(|c| c.better(p.req.margin));
            report.best = better.then(|| p.plan.setting(&found.best));
            report.phase = TunePhase::Done;
            if better && p.req.apply {
                match keep(ctx, p.k, &p.label, &p.plan, &found.best) {
                    Ok(()) => report.applied = true,
                    Err(e) => report.error = Some(e),
                }
            }
            if !report.applied {
                put_back(ctx, &p, &mut report);
            }
            let what = match (&report.best, report.applied) {
                (Some(b), true) => format!("{b}, kept"),
                (Some(b), false) => format!("{b} would be better; put back at {}", report.start),
                (None, _) => format!("none better than {}", report.start),
            };
            log::info!("{}: gain tuned: {what}", report.radio);
        }
        Err(Stop::Interrupted(why)) => {
            report.phase = if stop.lock().unwrap().is_some() { TunePhase::Stopped } else { TunePhase::Failed };
            log::info!("{}: gain tuning stopped ({why}); back at {}", report.radio, report.start);
            report.error = Some(why);
            put_back(ctx, &p, &mut report);
        }
    }
    // (Unless the recording ending took it already.)
    ctx.tune.active.lock().unwrap().take_if(|a| a.label == p.label);
    tell(ctx, &p.label, &report);
}

/// Back to what the run has (which a Setup edit meanwhile has changed, if so).
fn put_back(ctx: &Ctx, p: &Plan, report: &mut TuneReport) {
    let Some(now) = ctx.run.lock().unwrap().as_ref().and_then(|r| r.sources.get(p.k).cloned()) else { return };
    if let Err(e) = ctx.trial_gain(p.k, &now)
        && ctx.phase.lock().unwrap().phase == Phase::Running
    {
        report.error.get_or_insert(format!("putting the gain back failed ({e}): Setup has it as it was"));
    }
}

/// Keep `best` on run source `k` (labelled `label`): the run has it, and the config is saved with it.
fn keep(ctx: &Ctx, k: usize, label: &str, plan: &GainPlan, best: &Point) -> Result<(), String> {
    if let Some(r) = ctx.run.lock().unwrap().as_mut().and_then(|r| r.sources.get_mut(k)) {
        *r = plan.apply(r, best);
    }
    let mut found = false;
    crate::server::edit_config(ctx, |c| {
        if let Some(s) = c.sources.iter_mut().find(|s| s.label() == label) {
            *s = plan.apply(s, best);
            found = true;
        }
    })?;
    if found { Ok(()) } else { Err("the radio is gone from the config: the gain wasn't saved".into()) }
}

/// The ticks, and setting candidates on the radio.
struct Feed<'a> {
    ctx: &'a Ctx,
    p: &'a Plan,
    rx: &'a mpsc::Receiver<(f64, Values)>,
    stop: &'a Mutex<Option<String>>,
    /// The last second's values.
    last: Option<Values>,
}

impl Feed<'_> {
    /// The next second's values.
    fn tick(&mut self) -> Result<(f64, Values), String> {
        if let Some(why) = self.stop.lock().unwrap().clone() {
            return Err(why);
        }
        match self.rx.recv_timeout(Duration::from_secs(10)) {
            Ok((t, v)) => {
                self.last = Some(v.clone());
                Ok((t, v))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Err("the recording stopped sending stats".into()),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self.stop.lock().unwrap().clone().unwrap_or_else(|| "recording stopped".into())),
        }
    }

    /// `cand` against `inc`, by turns (inc first in even rounds, cand first in odd ones,
    /// so a trend over the minutes falls on both alike). A candidate refused in a slice
    /// ends it at once. Returns the comparison and each side's tallies, added up.
    fn compare(&mut self, inc: &Point, cand: &Point) -> Result<(Option<Cmp>, Step, Step), String> {
        let p = self.p;
        let (mut inc_all, mut cand_all) = (Step::default(), Step::default());
        let mut rounds = vec![];
        let mut slices: Vec<(Step, Step)> = vec![];
        let snrs = |slices: &[(Step, Step)]| -> Vec<(f64, f64)> { slices.iter().filter_map(|(i, c)| Some((i.snr_of(p.scored.as_deref())?, c.snr_of(p.scored.as_deref())?))).collect() };
        for r in 0..p.req.rounds {
            let (mut inc_slice, mut cand_slice) = (Step::default(), Step::default());
            let order = if r % 2 == 0 { [false, true] } else { [true, false] };
            let (mut si, mut sc) = (None, None);
            for is_cand in order {
                let step = if is_cand { self.slice(cand, false)? } else { self.slice(inc, true)? };
                let s = step.score_of(p.scored.as_deref());
                if is_cand {
                    cand_all.merge(&step);
                    if s.is_none() {
                        cand_all.abandoned = step.abandoned.clone();
                        rounds.push((si.flatten(), None));
                        return Ok((Cmp::of(&rounds), inc_all, cand_all));
                    }
                    sc = Some(s);
                    cand_slice = step;
                } else {
                    inc_all.merge(&step);
                    si = Some(s);
                    inc_slice = step;
                }
            }
            rounds.push((si.flatten(), sc.flatten()));
            slices.push((inc_slice, cand_slice));
            // The other systems: refused when one loses more than `protect` over the rounds, or
            // twice that in the first (so a setting that badly hurts one isn't kept on the air).
            if let Some(limit) = p.req.protect.filter(|_| !p.protected.is_empty()) {
                let limit = if r == 0 { 2.0 * limit } else { limit };
                if let Some(why) = tn::harmed(&slices, &p.protected, limit).filter(|_| r == 0 || r + 1 == p.req.rounds) {
                    cand_all.abandoned = Some(why);
                    return Ok((None, inc_all, cand_all));
                }
            }
        }
        Ok((Cmp::of(&rounds).map(|c| c.with_snr(&snrs(&slices))), inc_all, cand_all))
    }

    /// Sets `at`, lets it settle, and measures it for a slice. `incumbent`: the
    /// setting compared against, which at the start is the radio as it is.
    fn slice(&mut self, at: &Point, incumbent: bool) -> Result<Step, String> {
        let p = self.p;
        // The control channels decoding before the change: losing one of them gives the setting up.
        let decoding: Vec<String> = match &self.last {
            Some(v) => p.watches.iter().filter(|w| tn::locked(v, w)).map(|w| w.short_name.clone()).collect(),
            None => vec![],
        };
        let src = if incumbent && p.as_it_is && *at == p.start { p.base.clone() } else { p.plan.apply(&p.base, at) };
        self.ctx.trial_gain(p.k, &src)?;
        let mut step = Step::default();
        let mut quiet: BTreeMap<String, f64> = BTreeMap::new();
        let (mut last_t, mut settled) = (None, 0.0);
        loop {
            let (t, values) = self.tick()?;
            let dt = last_t.map_or(1.0, |l: f64| (t - l).clamp(0.2, 5.0));
            last_t = Some(t);
            // A control channel lost while settling counts toward giving the setting up.
            for w in &p.watches {
                let q = quiet.entry(w.short_name.clone()).or_default();
                *q = if tn::locked(&values, w) { 0.0 } else { *q + dt };
            }
            if let Some(why) = tn::went_quiet(&quiet, &decoding) {
                step.abandoned = Some(why);
                return Ok(step);
            }
            if settled < p.req.settle_s {
                settled += dt;
                continue;
            }
            tn::tally(&mut step, &p.watches, &p.label, &values, dt);
            if step.src.clip.get().is_some_and(|c| c > 4.0 * tn::CLIP_PCT) && step.src.seconds >= 5.0 {
                step.abandoned = Some(format!("clipping ({:.1}% of samples at full scale)", step.src.clip.get().unwrap_or(0.0)));
                return Ok(step);
            }
            if step.src.seconds >= p.req.slice_s {
                return Ok(step);
            }
        }
    }
}

// ───────────────────────────── trunk-pro tune ─────────────────────────────

const HELP: &str = "\
trunk-pro tune [--url http://127.0.0.1:8080] [--source N] [--systems a,b] [--slice 15] [--rounds 3]
               [--settle 4] [--max-steps 12] [--margin 1] [--mode manual|linearity|sensitivity]
               [--protect 5] [--try stage=value,…] [--yes] [--apply]

Asks a recorder that is recording to find the gain at which a radio decodes its
systems best. The recorder follows a plan for the radio's kind (which stage moves
first, which way, in what steps: an Airspy along its linearity table, an RTL-SDR
along its tuner's gains, a HackRF's LNA, then VGA, then amp; an Airspy through
SoapySDR along the same table, set as its LNA, MIX and VGA elements; another
SoapySDR radio by the gain elements it reported…). Each candidate is
compared with the best so far by turns, a --slice of seconds each (after --settle),
for --rounds rounds, and kept only if it scores better by more than --margin and by
more than twice the rounds' spread. Systems are scored on control channel decoding
first, then voice frames lost; where those tie (a control channel decoded in full),
a gain that lifts its SNR by more than 2 dB is better; a gain that clips is refused. Recording goes on
throughout, and nothing is saved: the radio goes back as it was at the end (or on
Ctrl-C) unless --apply keeps the best.

  --url        the recorder (its built-in interface's address)
  --source     the radio, by its number in Setup's list from 1 (needed with several)
  --systems    score only these systems; the radio's others are still watched
  --protect    with --systems: refuse a gain that costs one of the radio's other systems more
               than this many points (0–100 scale; default 5, `off` to allow it)
  --try        compare one setting with the one it has, no search: lna=11,mixer=10,vga=9
               (Airspy manual), step=16 (Airspy linearity/sensitivity), lna=32,vga=24,amp=on
               (HackRF), gain=33.8 (RTL-SDR, USRP, bladeRF, SoapySDR by its overall gain),
               step=14 (an Airspy through SoapySDR), lna=20,tia=9 (SoapySDR by its elements' names)
  --mode       Airspy, HydraSDR, an Airspy through SoapySDR: search this gain mode instead of
               the one it is in (through SoapySDR: linearity unless asked)
  --max-steps  at most this many candidates compared
  --margin     a move is kept only when it scores this much better (0–100 scale)
  --yes        don't ask before starting
  --apply      keep the best setting and save it
";

pub fn cli(a: &Args) {
    if a.flag("help") || a.positional.first().is_some_and(|p| p == "help") {
        print!("{HELP}");
        return;
    }
    if let Err(e) = client(a) {
        die(&format!("tune: {e}"));
    }
}

fn request(a: &Args) -> Result<TuneRequest, String> {
    let d = TuneRequest::default();
    let source = match a.get("source") {
        Some(s) => s.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).ok_or_else(|| format!("--source {s}: its number in Setup, from 1"))?,
        None => usize::MAX,
    };
    Ok(TuneRequest {
        source,
        systems: a.get("systems").map_or_else(Vec::new, |s| s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()),
        protect: match a.get("protect") {
            Some("off" | "0") => None,
            Some(v) => Some(v.parse().map_err(|_| format!("--protect {v}: points (0–100), or off"))?),
            None => d.protect,
        },
        slice_s: a.num("slice", d.slice_s),
        settle_s: a.num("settle", d.settle_s),
        rounds: a.num("rounds", d.rounds as f64) as usize,
        max_steps: a.num("max-steps", d.max_steps as f64) as usize,
        margin: a.num("margin", d.margin),
        mode: a.get("mode").map(|m| serde_json::from_value(m.to_ascii_lowercase().into()).map_err(|_| format!("--mode {m}: manual, linearity or sensitivity"))).transpose()?,
        try_setting: a.get("try").map(trunk_app::gain::Setting::parse).transpose().map_err(|e| format!("--try {e}"))?,
        apply: a.flag("apply"),
        check: false,
    })
}

/// A connection to a recorder's `/api/ws`.
struct Conn {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
}

impl Conn {
    /// Connected, and greeted: the greeting's messages.
    fn open(url: &str) -> Result<(Conn, Vec<serde_json::Value>), String> {
        let ws_url = ws_url(url)?;
        let (ws, _) = tungstenite::connect(&ws_url).map_err(|e| format!("couldn't connect to {ws_url}: {e}"))?;
        if let MaybeTlsStream::Plain(s) = ws.get_ref() {
            s.set_read_timeout(Some(Duration::from_millis(500))).map_err(|e| e.to_string())?;
        }
        let mut c = Conn { ws };
        let (mut greeting, until) = (vec![], Instant::now() + Duration::from_secs(15));
        while Instant::now() < until {
            match c.next()? {
                Some(m) if m["type"] == "hello" => return Ok((c, greeting)),
                Some(m) => greeting.push(m),
                None => {}
            }
        }
        Err("the recorder didn't finish greeting".into())
    }

    fn next(&mut self) -> Result<Option<serde_json::Value>, String> {
        match self.ws.read() {
            Ok(Message::Text(t)) => serde_json::from_str(t.as_str()).map(Some).map_err(|e| e.to_string()),
            Ok(Message::Close(_)) => Err("the recorder closed the connection".into()),
            Ok(_) => Ok(None),
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(None),
            Err(e) => Err(format!("connection to the recorder: {e}")),
        }
    }

    fn send(&mut self, cmd: serde_json::Value) -> Result<(), String> {
        self.ws.send(Message::text(cmd.to_string())).map_err(|e| format!("sending to the recorder: {e}"))
    }

    /// The next `tune` report (or the recorder's error).
    fn report(&mut self) -> Result<Option<TuneReport>, String> {
        let Some(m) = self.next()? else { return Ok(None) };
        match m["type"].as_str() {
            Some("tune") => serde_json::from_value(m).map(Some).map_err(|e| e.to_string()),
            Some("error") => Err(m["message"].as_str().unwrap_or("error").to_string()),
            _ => Ok(None),
        }
    }
}

/// `r` as the `tuneGain` command.
fn tune_cmd(r: &TuneRequest) -> serde_json::Value {
    let mut v = serde_json::to_value(r).unwrap_or_default();
    v["type"] = "tuneGain".into();
    v
}

fn client(a: &Args) -> Result<(), String> {
    let url = a.get("url").unwrap_or("http://127.0.0.1:8080");
    let mut req = request(a)?;
    // Ctrl-C: the recorder is asked to stop (it puts the radio back). A second one leaves at once.
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
            rt.block_on(async {
                let _ = tokio::signal::ctrl_c().await;
                eprintln!("\nStopping: the recorder puts the radio back (Ctrl-C again to leave at once)…");
                stop.store(true, Ordering::SeqCst);
                let _ = tokio::signal::ctrl_c().await;
                std::process::exit(130);
            });
        });
    }
    let (mut c, greeting) = Conn::open(url)?;
    if req.source == usize::MAX {
        let sources = greeting.iter().find(|m| m["type"] == "config").and_then(|m| m["config"]["sources"].as_array().cloned()).unwrap_or_default();
        match sources.len() {
            1 => req.source = 0,
            0 => return Err("the recorder's config has no sources".into()),
            n => return Err(format!("this recorder has {n} radios: say which with --source (1 to {n}, as in Setup)")),
        }
    }
    // What it would do, before anything changes.
    c.send(tune_cmd(&TuneRequest { check: true, ..req.clone() }))?;
    let plan = loop {
        if stop.load(Ordering::SeqCst) {
            return Err("left before starting; nothing was changed".into());
        }
        if let Some(r) = c.report()?.filter(|r| r.phase == TunePhase::Planned) {
            break r;
        }
    };
    println!("{}", intro(&plan, req.apply));
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) && !a.flag("yes") {
        println!("Press Enter to start, or Ctrl-C to leave without changing anything.");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        if stop.load(Ordering::SeqCst) {
            return Err("left before starting; nothing was changed".into());
        }
    }
    println!();
    c.send(tune_cmd(&req))?;
    let (mut shown, mut asked_stop) = (0, false);
    loop {
        if stop.load(Ordering::SeqCst) && !asked_stop {
            c.send(serde_json::json!({ "type": "tuneStop" }))?;
            asked_stop = true;
        }
        let Some(r) = c.report()? else { continue };
        if r.phase == TunePhase::Planned || r.source != req.source {
            continue;
        }
        for t in &r.tried[shown.min(r.tried.len())..] {
            println!("{}", line(t, req.margin));
        }
        shown = r.tried.len();
        if r.phase != TunePhase::Running {
            println!("\n{}", summary(&r, req.rounds));
            return match (r.phase, r.error) {
                (TunePhase::Done, None) => Ok(()),
                (_, e) => Err(e.unwrap_or_else(|| "stopped".into())),
            };
        }
    }
}

fn ws_url(url: &str) -> Result<String, String> {
    let u = url.trim().trim_end_matches('/');
    if u.starts_with("https://") || u.starts_with("wss://") {
        return Err("--url: https isn't supported; use http:// (or an SSH tunnel)".into());
    }
    let rest = u.strip_prefix("http://").or_else(|| u.strip_prefix("ws://")).unwrap_or(u);
    let host = rest.split('/').next().unwrap_or(rest);
    if host.is_empty() {
        return Err(format!("--url {url}: no host"));
    }
    let host = if host.contains(':') { host.to_string() } else { format!("{host}:8080") };
    Ok(format!("ws://{host}/api/ws"))
}

/// "a", "a and b", "a, b and c".
fn join_and(v: &[String]) -> String {
    match v {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Before anything changes: the radio, the plan, how long, and what it means for its systems.
fn intro(r: &TuneReport, apply: bool) -> String {
    let mut out = format!("Tuning {} from {}\n", r.radio, r.start);
    for l in &r.plan {
        out.push_str(&format!("  plan: {l}\n"));
    }
    out.push_str(&format!("  scoring: {}", r.scored.join(", ")));
    if !r.watched.is_empty() {
        out.push_str(&format!(" (also watching {})", r.watched.join(", ")));
    }
    let all: Vec<String> = r.scored.iter().chain(&r.watched).cloned().collect();
    out.push_str(&format!(
        "\n\nThis will take up to about {:.0} minutes. Recording and uploading carry on throughout, but {} may record worse while poorer settings are on the air, a minute or two at a time. {}",
        (r.estimate_s / 60.0).ceil().max(1.0),
        join_and(&all),
        if apply { "At the end it keeps the best gain, and saves it." } else { "At the end the radio goes back exactly as it was; nothing is saved (--apply keeps the best)." }
    ));
    out
}

/// One comparison: the candidate against the best so far, then each system
/// (the candidate's figures, the incumbent's in brackets).
fn line(t: &TuneTry, margin: f64) -> String {
    let verdict = match (&t.cmp, &t.why) {
        (None, Some(why)) => format!("refused: {why}"),
        (None, None) => "refused".into(),
        (Some(c), _) if t.final_check => format!("{:+5.1} ± {:.1}  final check against the start", c.diff, c.se),
        (Some(c), _) => format!("{:+5.1} ± {:.1}{}  {}", c.diff, c.se, snr_text(c), match (c.better(margin), c.by_snr(margin)) {
            (true, true) => "decodes as well, more SNR: moving to it",
            (true, false) => "better: moving to it",
            _ => "not clearly better",
        }),
    };
    let mut out = format!("  {}  vs {}:  {verdict}", t.candidate, t.against);
    for s in &t.systems {
        let star = if s.scored { "*" } else { " " };
        out.push_str(&format!("\n    {star} {:<16} control {:5.1}% ({:5.1}%)", s.system, s.control_pct[1], s.control_pct[0]));
        if let Some([was, now]) = s.voice_lost_pct {
            out.push_str(&format!(" · voice lost {now:.1}% ({was:.1}%)"));
        }
        match (s.phase_err, s.snr_db) {
            (Some([was, now]), _) => out.push_str(&format!(" · {now:.0}° ({was:.0}°)")),
            (None, Some([was, now])) => out.push_str(&format!(" · {now:.0} dB ({was:.0} dB)")),
            _ => {}
        }
    }
    if t.clip_pct > 0.0 {
        out.push_str(&format!("\n      radio: clipping {:.2}%", t.clip_pct));
    }
    out
}

/// The SNR tie-break's figures, after a score: " · SNR +6.8 ± 0.3 dB".
fn snr_text(c: &Cmp) -> String {
    c.snr.map_or_else(String::new, |s| format!(" · SNR {:+.1} ± {:.1} dB", s.diff, s.se))
}

/// What it found, and what was done with it.
fn summary(r: &TuneReport, rounds: usize) -> String {
    let mut out = format!("Summary ({:.0} min, {} settings compared)\n", r.elapsed_s / 60.0, r.tried.iter().filter(|t| !t.final_check).count());
    match (&r.best, r.tried.last()) {
        (Some(b), Some(t)) => {
            out.push_str(&format!("  Recommended: gain {b} (now {}).\n", r.start));
            if let Some(c) = t.cmp {
                out.push_str(&format!("  With it, measured against the current one by turns ({rounds} rounds): score {:+.1} ± {:.1}{}\n", c.diff, c.se, snr_text(&c)));
            }
            for s in &t.systems {
                let star = if s.scored { "*" } else { " " };
                let [c0, c1] = s.control_pct;
                out.push_str(&format!("    {star}{:<16} control channel {c0:.1}% → {c1:.1}% ({:+.1})", s.system, c1 - c0));
                if let Some([v0, v1]) = s.voice_lost_pct {
                    out.push_str(&format!(" · voice lost {v0:.1}% → {v1:.1}%"));
                }
                out.push('\n');
            }
        }
        _ if r.phase == TunePhase::Done => out.push_str(&format!("  No setting tried was clearly better than {}: keep it.\n", r.start)),
        _ => {}
    }
    out.push_str(&match (r.phase, r.applied, &r.best) {
        (TunePhase::Done, true, Some(b)) => format!("\nApplied: the gain {b} is set now and saved."),
        (TunePhase::Done, false, Some(_)) => format!("\nNothing was changed: the radio is back at {}. Run again with --apply, or set it in Setup.", r.start),
        (TunePhase::Done, _, None) => format!("\nThe gain is back at {}.", r.start),
        _ => format!("\nStopped ({}): the radio is back at {}.", r.error.as_deref().unwrap_or("asked to"), r.start),
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_best_kept_is_what_the_run_has_and_what_is_saved() {
        let dir = crate::db::TestDir::new("tune-keep");
        let ctx = crate::runtime::test_ctx(&dir);
        let rtl: Source = serde_json::from_value(serde_json::json!({ "type": "rtlsdr", "serial": "200", "gainDb": 28, "agc": true, "centerHz": 858.3e6, "rateHz": 2.4e6, "name": "Roof" })).unwrap();
        ctx.config.lock().unwrap().sources = vec![rtl.clone()];
        let mut run = ctx.config.lock().unwrap().clone();
        run.sources[0].name.clear();
        *ctx.run.lock().unwrap() = Some(run);
        let plan = GainPlan::of(&rtl, None).unwrap();
        let best = plan.point_of(&plan.point(&rtl), &trunk_app::gain::Setting::parse("gain=33.8").unwrap()).unwrap();
        keep(&ctx, 0, &rtl.label(), &plan, &best).unwrap();
        let gain = |s: &Source| match s.kind {
            crate::config::SourceKind::Rtlsdr { gain_db, agc, .. } => (gain_db, agc),
            _ => panic!(),
        };
        assert_eq!(gain(&ctx.run.lock().unwrap().as_ref().unwrap().sources[0]), (33.8, false));
        let saved = ctx.config.lock().unwrap().sources[0].clone();
        assert_eq!((gain(&saved), saved.name.as_str()), ((33.8, false), "Roof"), "the gain changed, nothing else");
        let file = std::fs::read_to_string(dir.join("config.json")).unwrap();
        assert!(file.contains("33.8"), "{file}");
    }

    #[test]
    fn a_url_is_the_recorders_api() {
        assert_eq!(ws_url("http://pi:8080/").unwrap(), "ws://pi:8080/api/ws");
        assert_eq!(ws_url("192.168.1.104").unwrap(), "ws://192.168.1.104:8080/api/ws");
        assert!(ws_url("https://x").is_err());
    }

    #[test]
    fn the_command_line_is_a_tune_request() {
        let argv: Vec<String> = ["--source", "2", "--try", "lna=11,vga=9", "--protect", "off", "--mode", "Manual"].map(String::from).to_vec();
        let r = request(&Args::parse(&argv)).unwrap();
        assert_eq!((r.source, r.protect, r.mode), (1, None, Some(trunk_app::config::AirspyGain::Manual)));
        assert_eq!(r.try_setting.unwrap().to_string(), "LNA 11 · VGA 9");
        let cmd = tune_cmd(&TuneRequest::default());
        assert_eq!(cmd["type"], "tuneGain");
        let back: TuneRequest = serde_json::from_value(cmd).unwrap();
        assert_eq!(back, TuneRequest::default(), "the recorder reads what the command line sends");
    }
}
