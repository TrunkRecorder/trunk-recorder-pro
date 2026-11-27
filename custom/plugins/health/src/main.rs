//! Health: a Trunk Recorder Pro plugin that checks the recorder's health
//! every few minutes — radios, control channels, calls, uploads, disk, clock —
//! says on its card what to look at and what to do, logs each finding, and
//! sends phone alerts through ntfy when the state changes.

mod alert;
mod api;
mod rules;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use trunk_recorder_plugin::{
    Action, CallRecord, CallsPage, CallsQuery, Endpoint, EndpointState, Host, Manifest, Metrics, Page, Plugin, Reply, Setup, State, Status, topic,
};

use crate::rules::Level;

/// The recorder said "shutdown": anything else that ends our input means it vanished.
static TOLD_TO_STOP: AtomicBool = AtomicBool::new(false);

// ── settings ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
#[schemars(extend("x-actions" = [
    { "id": "test", "label": "Send a test alert", "hint": "Sends one to the ntfy topic above, as typed" },
    { "id": "check", "label": "Check now", "hint": "Runs the checks now and says what it found" }
]))]
struct Config {
    /// ntfy topic for alerts
    ///
    /// Where phone alerts go: a topic name (sent to ntfy.sh) or a full URL
    /// (https://ntfy.sh/your-topic, or your own ntfy server). In the ntfy phone
    /// app, subscribe to the same topic. Pick a name nobody would guess: anyone
    /// who knows it can read it. Empty: no alerts.
    ntfy: String,
    /// ntfy access token
    ///
    /// Only for an ntfy server (or topic) that needs one.
    #[schemars(extend("x-secret" = true))]
    ntfy_token: String,
    /// Alert on
    alert_on: AlertOn,
    /// Checks in a row before alerting
    ///
    /// A change has to hold this many checks before it's told, so one bad
    /// minute doesn't wake you.
    #[schemars(range(min = 1, max = 10))]
    alert_after: u32,
    /// Check every (minutes)
    #[schemars(range(min = 1, max = 60))]
    every_min: u32,
    /// Look back (minutes)
    ///
    /// The span each check judges: calls, drops, clipping and decode rates over it.
    #[schemars(range(min = 10, max = 240))]
    window_min: u32,
    /// The recorder's web port
    ///
    /// 0: found by itself (from the config file).
    port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Config { ntfy: String::new(), ntfy_token: String::new(), alert_on: AlertOn::Warn, alert_after: 2, every_min: 5, window_min: 30, port: 0 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum AlertOn {
    /// WARN and BAD
    #[default]
    Warn,
    /// BAD only
    Bad,
    /// Never (the card and log only)
    Never,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
struct SystemConfig {
    /// Don't check this system
    ///
    /// Leave its calls and control channels out of the health check (an
    /// airband or weather channel you know is weak, say). Its radio is still checked.
    skip: bool,
}

// ── the plugin ───────────────────────────────────────────────────────────────

struct Shared {
    host: Host,
    data_dir: PathBuf,
    config: Mutex<Config>,
    /// Systems not checked, by short name (lower case), and as named.
    skip: Mutex<(HashSet<String>, Vec<String>)>,
    /// Calls queries waiting for their page.
    pending: Mutex<HashMap<u64, Sender<CallsPage>>>,
    /// Control channels from the status events: "system/site" → Hz.
    control: Mutex<BTreeMap<String, f64>>,
}

enum Cmd {
    Check(Option<Reply>),
    Stop,
}

struct Health {
    shared: Arc<Shared>,
    tx: Sender<Cmd>,
    worker: Option<std::thread::JoinHandle<()>>,
}

fn skips(setup: &Setup<Config, SystemConfig>) -> (HashSet<String>, Vec<String>) {
    let names: Vec<String> = setup.systems.iter().filter(|s| s.config.as_ref().is_some_and(|c| c.skip)).map(|s| s.short_name.clone()).collect();
    (names.iter().map(|n| n.to_lowercase()).collect(), names)
}

impl Plugin for Health {
    type Config = Config;
    type SystemConfig = SystemConfig;

    fn manifest() -> Manifest {
        Manifest {
            name: "Health check".into(),
            subscribe: vec![topic::STATUS.into()],
            page: Some(Page { title: "Health".into() }),
            live_settings: true,
            ..trunk_recorder_plugin::manifest!()
        }
    }

    fn start(host: Host, setup: Setup<Config, SystemConfig>) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            host: host.clone(),
            data_dir: setup.data_dir.clone(),
            config: Mutex::new(setup.config.clone()),
            skip: Mutex::new(skips(&setup)),
            pending: Mutex::new(HashMap::new()),
            control: Mutex::new(BTreeMap::new()),
        });
        host.status(State::Ok, "Starting: the first check runs in a minute");
        // Back after vanishing: say so.
        let mut mem = alert::load(&shared.data_dir);
        if mem.went_away {
            mem.went_away = false;
            alert::save(&shared.data_dir, &mem);
            let c = setup.config.clone();
            let host2 = host.clone();
            std::thread::spawn(move || {
                if !c.ntfy.trim().is_empty() && c.alert_on != AlertOn::Never {
                    let h = hostname();
                    match alert::ntfy(&c.ntfy, &c.ntfy_token, &format!("{h}: TRP is recording again"), &format!("Trunk Recorder Pro on {h} is back and recording."), Level::Ok) {
                        Ok(()) => host2.info("Told ntfy the recorder is back"),
                        Err(e) => host2.warn(format!("ntfy: {e}")),
                    }
                }
            });
        }
        let (tx, rx) = channel();
        let s2 = shared.clone();
        let worker = std::thread::spawn(move || work(s2, rx));
        Ok(Health { shared, tx, worker: Some(worker) })
    }

    fn status(&mut self, st: Status) {
        let mut m = self.shared.control.lock().unwrap();
        for s in st.systems {
            if let Some(hz) = s.control_channel_hz {
                let name = if s.site_name.is_empty() || s.site_name == s.short_name { s.short_name.clone() } else { format!("{}/{}", s.short_name, s.site_name) };
                m.insert(name, hz as f64);
            }
        }
    }

    fn calls(&mut self, request: u64, page: CallsPage) {
        if let Some(tx) = self.shared.pending.lock().unwrap().remove(&request) {
            let _ = tx.send(page);
        }
    }

    fn action(&mut self, a: Action, reply: Reply) {
        match a.action.as_str() {
            "test" => {
                // The settings as typed (saved or not).
                let c: Config = serde_json::from_value(a.value).unwrap_or_else(|_| self.shared.config.lock().unwrap().clone());
                std::thread::spawn(move || {
                    let h = hostname();
                    match alert::ntfy(&c.ntfy, &c.ntfy_token, &format!("{h}: health check test"), "If you can read this, alerts from Trunk Recorder Pro work.", Level::Ok) {
                        Ok(()) => reply.ok("Sent: check your phone"),
                        Err(e) => reply.fail(format!("Not sent: {e}")),
                    }
                });
            }
            "check" => {
                let _ = self.tx.send(Cmd::Check(Some(reply)));
            }
            _ => reply.fail("No such button"),
        }
    }

    fn settings(&mut self, setup: Setup<Config, SystemConfig>) -> Result<(), String> {
        *self.shared.config.lock().unwrap() = setup.config.clone();
        *self.shared.skip.lock().unwrap() = skips(&setup);
        let _ = self.tx.send(Cmd::Check(None));
        Ok(())
    }

    fn shutdown(&mut self, _grace: Duration) {
        let _ = self.tx.send(Cmd::Stop);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        if TOLD_TO_STOP.load(Ordering::SeqCst) {
            return;
        }
        // Our input ended without a shutdown: the recorder crashed or was killed.
        let mut mem = alert::load(&self.shared.data_dir);
        mem.went_away = true;
        alert::save(&self.shared.data_dir, &mem);
        let c = self.shared.config.lock().unwrap().clone();
        if !c.ntfy.trim().is_empty() && c.alert_on != AlertOn::Never {
            let h = hostname();
            let _ = alert::ntfy(
                &c.ntfy,
                &c.ntfy_token,
                &format!("{h}: TRP stopped unexpectedly"),
                &format!("Trunk Recorder Pro on {h} went away without stopping recording (a crash, or it was killed).\n\nCheck: systemctl --user status trunk-pro\nand: journalctl --user -u trunk-pro -n 50"),
                Level::Bad,
            );
        }
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "recorder".into())
}

fn now_s() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

// ── the checks, on a thread of their own ─────────────────────────────────────

fn work(sh: Arc<Shared>, rx: Receiver<Cmd>) {
    let mut before: HashMap<String, (Level, String)> = HashMap::new();
    let mut first = true;
    loop {
        let every = Duration::from_secs(60 * sh.config.lock().unwrap().every_min.clamp(1, 60) as u64);
        let wait = if first { Duration::from_secs(60) } else { every };
        let reply = match rx.recv_timeout(wait) {
            Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(Cmd::Check(r)) => r,
            Err(RecvTimeoutError::Timeout) => None,
        };
        let line = run_check(&sh, &mut before, first);
        first = false;
        if let Some(r) = reply {
            r.ok(line);
        }
    }
}

/// The recorded calls since `from_ms`, a page at a time.
fn fetch_calls(sh: &Shared, from_ms: i64) -> Vec<CallRecord> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..20 {
        let (tx, rx) = channel();
        {
            let mut p = sh.pending.lock().unwrap();
            let id = sh.host.calls(CallsQuery { from: Some(from_ms), cursor: cursor.clone(), limit: Some(500), ..Default::default() });
            p.insert(id, tx);
        }
        let Ok(page) = rx.recv_timeout(Duration::from_secs(20)) else { break };
        out.extend(page.entries.into_iter().map(|e| e.record));
        match page.next {
            Some(n) => cursor = Some(n),
            None => break,
        }
    }
    out
}

fn run_check(sh: &Shared, before: &mut HashMap<String, (Level, String)>, first: bool) -> String {
    let c = sh.config.lock().unwrap().clone();
    let window_s = 60 * c.window_min.clamp(10, 240) as u64;
    let (skip, skip_names) = sh.skip.lock().unwrap().clone();

    let port = api::find_port(c.port, &sh.data_dir);
    let live = match port {
        Some(p) => api::gather(p, window_s),
        None => Err("couldn't find the recorder's web port (set it in the settings)".into()),
    };
    let calls = fetch_calls(sh, ((now_s() - window_s as f64) * 1000.0) as i64);
    let control: Vec<(f64, String)> = sh.control.lock().unwrap().iter().map(|(n, h)| (*h, n.clone())).collect();
    let me = trunk_recorder_plugin::manifest!().id;
    let report = rules::check(&rules::Input { live: live.as_ref().ok(), calls: &calls, skip: &skip, window_s: window_s as f64, control_channels: &control, me: &me });

    // The card
    let mut line = rules::status_line(&report, &skip_names);
    let state = match report.overall {
        Level::Bad => State::Error,
        Level::Warn => State::Warning,
        _ => State::Ok,
    };
    if let Err(e) = &live {
        line = format!("{line} — radio, computer and upload checks skipped: {e}");
    }
    sh.host.status(if live.is_err() && state == State::Ok { State::Warning } else { state }, line.clone());

    // The log: what's new, changed or cleared
    if first {
        sh.host.info(format!("First check over the last {} minutes: {line}", c.window_min));
    }
    let mut now: HashMap<String, (Level, String)> = HashMap::new();
    for x in &report.findings {
        now.insert(x.key.clone(), (x.level, x.title.clone()));
        if before.get(&x.key).map(|b| b.0) != Some(x.level) {
            let l = rules::log_line(x);
            match x.level {
                Level::Bad => sh.host.error(l),
                Level::Warn => sh.host.warn(l),
                _ => sh.host.info(l),
            }
        }
    }
    for (k, (lvl, title)) in before.iter() {
        if !now.contains_key(k) && *lvl >= Level::Warn {
            sh.host.info(format!("Cleared — {title}"));
        }
    }
    *before = now;

    // Alerts
    let mut ntfy_state = None;
    let floor = match c.alert_on {
        AlertOn::Warn => Some(Level::Warn),
        AlertOn::Bad => Some(Level::Bad),
        AlertOn::Never => None,
    };
    if let (Some(floor), false) = (floor, c.ntfy.trim().is_empty()) {
        let mut mem = alert::load(&sh.data_dir);
        if let Some(lvl) = alert::decide(&mut mem, report.overall, floor, c.alert_after) {
            let h = hostname();
            let (title, body) = if lvl == Level::Ok {
                (format!("{h}: TRP back to OK"), format!("Trunk Recorder Pro on {h} is healthy again (was {}).", mem.reported))
            } else {
                let items: Vec<String> = report.problems().iter().filter(|x| x.level >= floor).take(5).map(|x| format!("{}: {}", x.level.word(), x.title)).collect();
                (format!("{h}: TRP {}", lvl.word()), format!("{}\n\nDetails: the Health page in Trunk Recorder Pro on {h}.", items.join("\n")))
            };
            match alert::ntfy(&c.ntfy, &c.ntfy_token, &title, &body, lvl) {
                Ok(()) => {
                    sh.host.info(format!("Alert sent: {title}"));
                    mem.reported = lvl.word().into();
                    mem.seen.clear();
                    mem.count = 0;
                    ntfy_state = Some(Ok(()));
                }
                Err(e) => {
                    sh.host.warn(format!("Alert not sent (tried again next check): {e}"));
                    ntfy_state = Some(Err(e));
                }
            }
        }
        alert::save(&sh.data_dir, &mem);
    }

    // The Health tab
    let mut endpoints = vec![Endpoint {
        name: format!("Recorder web API{}", port.map(|p| format!(" (port {p})")).unwrap_or_default()),
        state: if live.is_ok() { EndpointState::Up } else { EndpointState::Down },
        last_error: live.as_ref().err().cloned().unwrap_or_default(),
        ..Default::default()
    }];
    if let Some(r) = ntfy_state {
        endpoints.push(Endpoint {
            name: "ntfy".into(),
            state: if r.is_ok() { EndpointState::Up } else { EndpointState::Down },
            last_error: r.err().unwrap_or_default(),
            ..Default::default()
        });
    }
    let mut extra = BTreeMap::new();
    extra.insert("overall".into(), report.overall.word().into());
    extra.insert("to look at".into(), report.problems().len().into());
    extra.insert("calls checked".into(), report.calls.into());
    for n in &report.notes {
        if let Some((k, v)) = n.split_once(' ').and_then(|(a, rest)| rest.split_once(' ').map(|(b, v)| (format!("{a} {b}"), v.to_string()))) {
            extra.insert(k, v.into());
        }
    }
    sh.host.metrics(&Metrics { last_ok: live.is_ok().then(now_s), last_error: live.is_err().then(now_s), endpoints, extra, ..Default::default() });
    line
}

// ── main: the SDK's loop, with an ear on how our input ends ──────────────────

fn main() {
    if std::env::args().nth(1).is_some() {
        // --describe, --version: the SDK answers.
        trunk_recorder_plugin::run::<Health>();
        return;
    }
    // Stay up through a SIGTERM to the whole service: the recorder tells us to
    // stop (or our input ends) — that's what we go by.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    let (reader, mut writer) = match std::io::pipe() {
        Ok(p) => p,
        Err(_) => {
            trunk_recorder_plugin::run::<Health>();
            return;
        }
    };
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            if line.contains("\"shutdown\"") && serde_json::from_str::<serde_json::Value>(&line).is_ok_and(|v| v["type"] == "shutdown") {
                TOLD_TO_STOP.store(true, Ordering::SeqCst);
            }
            if writer.write_all(line.as_bytes()).and_then(|_| writer.write_all(b"\n")).is_err() {
                break;
            }
        }
        // Dropping the writer ends the SDK's input.
    });
    let code = trunk_recorder_plugin::serve::<Health>(BufReader::new(reader), Host::to_writer(std::io::stdout()));
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;
    use trunk_recorder_plugin::{HostMessage, testing};

    #[test]
    fn its_settings_form_has_the_buttons_and_labels() {
        let m = trunk_recorder_plugin::describe::<Health>();
        let c = m.config.unwrap();
        assert_eq!(c["x-actions"][0]["id"], "test");
        assert_eq!(c["properties"]["alertOn"]["enum"], serde_json::json!(["warn", "bad", "never"]));
        assert_eq!(c["properties"]["ntfyToken"]["x-secret"], true);
        assert_eq!(m.system_config.unwrap()["properties"]["skip"]["type"], "boolean");
        assert_eq!(m.page.unwrap().title, "Health");
    }

    #[test]
    fn it_starts_and_stops() {
        let dir = testing::temp_dir("health");
        let hello = testing::hello(&dir, serde_json::json!({ "port": 1 }));
        let out = testing::run::<Health>([HostMessage::Hello(hello)]);
        assert!(out.ready());
        assert_eq!(out.exit_code, 0);
    }
}
