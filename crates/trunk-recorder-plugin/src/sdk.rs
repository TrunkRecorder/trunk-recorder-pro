//! Writing a plugin: implement [`Plugin`] and call [`run`] from `main`.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::protocol::*;
use crate::schema;

/// A plugin. Every method runs on the one thread that reads the recorder's
/// messages, one message at a time: return quickly, and hand slow work
/// (network, disk, other processes) to a thread of your own.
pub trait Plugin: Sized {
    /// The plugin's settings. `#[serde(default)]` on the struct lets a user
    /// leave any of them empty. The settings form is drawn from its schema
    /// (see [`crate::schema`]). [`NoConfig`] for none.
    type Config: DeserializeOwned + JsonSchema;
    /// Settings for each system, when the plugin needs some (an API key per
    /// system, say). [`NoConfig`] for none.
    type SystemConfig: DeserializeOwned + JsonSchema;

    /// Who the plugin is and what it subscribes to — start from [`crate::manifest!`].
    /// `config` and `system_config` are filled in from the types above.
    fn manifest() -> Manifest;

    /// Start with the user's settings. An `Err` is shown to the user, and the
    /// plugin isn't started again until recording next starts.
    fn start(host: Host, setup: Setup<Self::Config, Self::SystemConfig>) -> Result<Self, String>;

    fn call_start(&mut self, _call: CallInfo) {}
    fn call_end(&mut self, _call: CallInfo) {}
    fn call_concluded(&mut self, _call: ConcludedCall) {}
    fn unit(&mut self, _event: UnitEvent) {}
    fn audio(&mut self, _chunk: AudioChunk) {}
    fn status(&mut self, _status: Status) {}

    /// The recorder is stopping (or the plugin's stdin closed): finish or save
    /// what's in flight within `grace` — the process exits when this returns.
    fn shutdown(&mut self, _grace: std::time::Duration) {}
}

pub use crate::schema::NoConfig;

/// What [`Plugin::start`] gets.
#[derive(Clone, Debug)]
pub struct Setup<C, S> {
    pub config: C,
    /// Every system, with the plugin's settings for it (`None`: left empty).
    pub systems: Vec<System<S>>,
    pub host: HostInfo,
    pub capture_dir: PathBuf,
    /// The plugin's own folder, kept across restarts and upgrades.
    pub data_dir: PathBuf,
    /// Audio formats `call.concluded` will carry (always "wav").
    pub audio_formats: Vec<String>,
}

impl<C, S> Setup<C, S> {
    pub fn system(&self, index: u16) -> Option<&System<S>> {
        self.systems.iter().find(|s| s.index == index)
    }
    pub fn system_named(&self, short_name: &str) -> Option<&System<S>> {
        self.systems.iter().find(|s| s.short_name == short_name)
    }
    /// Whether `call.concluded` will carry `format` ([`format`]).
    pub fn has_format(&self, format: &str) -> bool {
        self.audio_formats.iter().any(|f| f == format)
    }
}

#[derive(Clone, Debug)]
pub struct System<S> {
    pub index: u16,
    pub short_name: String,
    /// "p25" | "smartnet" | "conventional"
    pub kind: String,
    pub config: Option<S>,
}

/// The way back to the recorder: logs, health, results. Cheap to clone and
/// usable from any thread. (A plugin's stdout belongs to the protocol — never
/// `println!` in a plugin; use this, or `eprintln!` for the raw log.)
#[derive(Clone)]
pub struct Host {
    out: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl Host {
    /// A host writing to `w` instead of stdout (see [`crate::testing`]).
    pub fn to_writer(w: impl Write + Send + 'static) -> Host {
        Host { out: Arc::new(Mutex::new(Box::new(w))) }
    }

    pub fn send(&self, m: &PluginMessage) {
        let mut line = serde_json::to_string(m).unwrap_or_default();
        line.push('\n');
        let mut o = self.out.lock().unwrap_or_else(|e| e.into_inner());
        // The recorder has gone when this fails; stdin's end will stop us.
        let _ = o.write_all(line.as_bytes()).and_then(|_| o.flush());
    }

    pub fn log(&self, level: Level, message: impl Into<String>) {
        self.send(&PluginMessage::Log { level, message: message.into() });
    }
    pub fn error(&self, message: impl Into<String>) {
        self.log(Level::Error, message)
    }
    pub fn warn(&self, message: impl Into<String>) {
        self.log(Level::Warn, message)
    }
    pub fn info(&self, message: impl Into<String>) {
        self.log(Level::Info, message)
    }
    pub fn debug(&self, message: impl Into<String>) {
        self.log(Level::Debug, message)
    }

    /// The plugin's health, shown in the interface until the next.
    pub fn status(&self, state: State, message: impl Into<String>) {
        self.send(&PluginMessage::Status { state, message: message.into() });
    }

    /// What became of a concluded call.
    pub fn call_result(&self, path: &str, outcome: Outcome, message: impl Into<String>, url: impl Into<String>) {
        self.send(&PluginMessage::CallResult { path: path.to_string(), outcome, message: message.into(), url: url.into() });
    }
}

/// The manifest with the settings schemas filled in.
pub fn describe<P: Plugin>() -> Manifest {
    let mut m = P::manifest();
    m.api = API_VERSION;
    let c = schema::schema_for::<P::Config>();
    m.config = (!schema::is_empty(&c)).then_some(c);
    let s = schema::schema_for::<P::SystemConfig>();
    m.system_config = (!schema::is_empty(&s)).then_some(s);
    m
}

/// Run the plugin: `--describe` prints its manifest; otherwise it talks to the
/// recorder on stdin/stdout until told to stop. Call from `main`.
pub fn run<P: Plugin>() {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("--describe") => {
            println!("{}", serde_json::to_string_pretty(&describe::<P>()).unwrap_or_default());
            return;
        }
        Some("--version" | "-V") => {
            println!("{}", P::manifest().version);
            return;
        }
        Some(a) => {
            eprintln!("unknown argument {a} (a plugin takes --describe or --version; the recorder runs it with none)");
            std::process::exit(2);
        }
        None => {}
    }
    let stdin = io::stdin();
    if stdin.is_terminal() {
        let m = P::manifest();
        eprintln!(
            "{} {} is a Trunk Recorder Lite plugin: the recorder runs it and talks to it over stdin/stdout.\n\
             `--describe` prints its manifest. Try it with `trunk-lite plugin run <this binary>`,\n\
             or paste protocol lines here (a hello first).",
            m.name, m.version
        );
    }
    let code = serve::<P>(stdin.lock(), Host { out: Arc::new(Mutex::new(Box::new(io::stdout()))) });
    std::process::exit(code);
}

/// The message loop over `input`; the process's exit status.
pub fn serve<P: Plugin>(input: impl BufRead, host: Host) -> i32 {
    let mut plugin: Option<P> = None;
    let mut grace = std::time::Duration::from_secs(10);
    for line in input.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg: HostMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                host.warn(format!("unreadable message from the recorder ({e}): {line}"));
                continue;
            }
        };
        let Some(p) = plugin.as_mut() else {
            let HostMessage::Hello(hello) = msg else {
                host.warn("a message before hello, ignored");
                continue;
            };
            match start::<P>(host.clone(), hello) {
                Ok(p) => {
                    plugin = Some(p);
                    host.send(&PluginMessage::Ready);
                }
                Err(e) => {
                    host.status(State::Error, e.clone());
                    host.error(e);
                    return EXIT_CONFIG;
                }
            }
            continue;
        };
        match msg {
            HostMessage::Hello(_) => host.warn("a second hello, ignored"),
            HostMessage::CallStart(c) => p.call_start(c),
            HostMessage::CallEnd(c) => p.call_end(c),
            HostMessage::CallConcluded(c) => p.call_concluded(c),
            HostMessage::Unit(u) => p.unit(u),
            HostMessage::Audio(a) => p.audio(a),
            HostMessage::Status(s) => p.status(s),
            HostMessage::Shutdown(s) => {
                grace = std::time::Duration::from_secs_f64(s.grace_s.max(0.0));
                break;
            }
            HostMessage::Unknown => {}
        }
    }
    if let Some(p) = plugin.as_mut() {
        p.shutdown(grace);
    }
    0
}

fn start<P: Plugin>(host: Host, hello: Hello) -> Result<P, String> {
    if hello.api != 0 && hello.api < API_VERSION {
        return Err(format!("this plugin needs a newer recorder (plugin API {API_VERSION}, the recorder has {})", hello.api));
    }
    let config: P::Config = parse(hello.config, "settings")?;
    let mut systems = Vec::new();
    for s in hello.systems {
        let config = match s.config {
            Value::Null => None,
            v => Some(parse::<P::SystemConfig>(v, &format!("settings for {}", s.short_name))?),
        };
        systems.push(System { index: s.index, short_name: s.short_name, kind: s.kind, config });
    }
    let setup = Setup { config, systems, host: hello.host, capture_dir: hello.capture_dir, data_dir: hello.data_dir, audio_formats: hello.audio_formats };
    P::start(host, setup)
}

/// Settings left out entirely count as `{}`, so `#[serde(default)]` applies.
fn parse<T: DeserializeOwned>(v: Value, what: &str) -> Result<T, String> {
    let v = if v.is_null() { Value::Object(Default::default()) } else { v };
    serde_json::from_value(v).map_err(|e| format!("bad {what}: {e}"))
}

/// A manifest with the id, version, description, homepage, repository,
/// authors and license from Cargo.toml; the name is the id until you set one:
///
/// ```ignore
/// fn manifest() -> Manifest {
///     Manifest { name: "OpenMHz".into(), subscribe: vec![topic::CALL_CONCLUDED.into()], ..trunk_recorder_plugin::manifest!() }
/// }
/// ```
#[macro_export]
macro_rules! manifest {
    () => {{
        let authors: Vec<String> = env!("CARGO_PKG_AUTHORS").split(':').filter(|a| !a.is_empty()).map(String::from).collect();
        $crate::Manifest {
            id: env!("CARGO_PKG_NAME").to_string(),
            name: env!("CARGO_PKG_NAME").to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            description: env!("CARGO_PKG_DESCRIPTION").to_string(),
            api: $crate::API_VERSION,
            homepage: env!("CARGO_PKG_HOMEPAGE").to_string(),
            repository: env!("CARGO_PKG_REPOSITORY").to_string(),
            authors,
            license: env!("CARGO_PKG_LICENSE").to_string(),
            ..Default::default()
        }
    }};
}
