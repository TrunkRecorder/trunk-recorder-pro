//! Testing a plugin without the recorder: feed it messages, look at what it
//! says back.
//!
//! ```no_run
//! # use trunk_recorder_plugin::{testing, Outcome, HostMessage};
//! # struct MyPlugin;
//! # impl trunk_recorder_plugin::Plugin for MyPlugin {
//! #   type Config = trunk_recorder_plugin::NoConfig; type SystemConfig = trunk_recorder_plugin::NoConfig;
//! #   fn manifest() -> trunk_recorder_plugin::Manifest { Default::default() }
//! #   fn start(_: trunk_recorder_plugin::Host, _: trunk_recorder_plugin::Setup<Self::Config, Self::SystemConfig>) -> Result<Self, String> { Ok(MyPlugin) }
//! # }
//! let dir = testing::temp_dir("my-plugin");
//! let hello = testing::hello(&dir, serde_json::json!({ "apiKey": "k" }));
//! let call = testing::call(&dir, "sys1", 101);
//! let out = testing::run::<MyPlugin>([HostMessage::Hello(hello), HostMessage::CallConcluded(call)]);
//! assert!(out.ready());
//! assert_eq!(out.results()[0].1, Outcome::Ok);
//! ```

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::protocol::*;
use crate::sdk::{serve, Host, Plugin};

/// What a plugin said, and how it ended.
#[derive(Debug, Default)]
pub struct Output {
    pub messages: Vec<PluginMessage>,
    /// The exit status `run` would have ended the process with.
    pub exit_code: i32,
}

impl Output {
    /// It started (its settings were good).
    pub fn ready(&self) -> bool {
        self.messages.iter().any(|m| matches!(m, PluginMessage::Ready))
    }
    /// (path, outcome, message, url) of each call result.
    pub fn results(&self) -> Vec<(String, Outcome, String, String)> {
        self.messages
            .iter()
            .filter_map(|m| match m {
                PluginMessage::CallResult { path, outcome, message, url } => Some((path.clone(), *outcome, message.clone(), url.clone())),
                _ => None,
            })
            .collect()
    }
    /// (level, message) of each log line.
    pub fn logs(&self) -> Vec<(Level, String)> {
        self.messages
            .iter()
            .filter_map(|m| match m {
                PluginMessage::Log { level, message } => Some((*level, message.clone())),
                _ => None,
            })
            .collect()
    }
    /// The last status it reported.
    pub fn status(&self) -> Option<(State, String)> {
        self.messages.iter().rev().find_map(|m| match m {
            PluginMessage::Status { state, message } => Some((*state, message.clone())),
            _ => None,
        })
    }
}

#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run `P` on `messages` (a hello first) as the recorder would, then end its
/// input — so its [`Plugin::shutdown`] runs — and collect what it said.
pub fn run<P: Plugin>(messages: impl IntoIterator<Item = HostMessage>) -> Output {
    let mut input = String::new();
    for m in messages {
        input.push_str(&serde_json::to_string(&m).expect("serializable"));
        input.push('\n');
    }
    let buf = Shared::default();
    let exit_code = serve::<P>(Cursor::new(input), Host::to_writer(buf.clone()));
    let bytes = buf.0.lock().unwrap().clone();
    parse(&bytes, exit_code)
}

/// A [`Host`] whose messages are kept, for testing parts of a plugin (like a
/// [`crate::CallQueue`]) on their own.
pub fn capture() -> (Host, Captured) {
    let buf = Shared::default();
    (Host::to_writer(buf.clone()), Captured(buf))
}

/// What a [`capture`] host was sent.
pub struct Captured(Shared);

impl Captured {
    pub fn output(&self) -> Output {
        parse(&self.0 .0.lock().unwrap(), 0)
    }
}

fn parse(bytes: &[u8], exit_code: i32) -> Output {
    let messages = String::from_utf8_lossy(bytes).lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    Output { messages, exit_code }
}

/// A fresh, empty folder for a test (under the system's temp folder).
pub fn temp_dir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let d = std::env::temp_dir().join(format!("{name}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

/// A hello with `config` and one P25 system, "sys1" (index 0), with no
/// settings of its own — set `systems[0].config` for those. Calls are under
/// `dir`, and the plugin's data folder is `dir/data`. Calls come as WAV and
/// M4A; for a recorder without an M4A encoder, set `audio_formats` to `["wav"]`.
pub fn hello(dir: &Path, config: Value) -> Hello {
    let data_dir = dir.join("data");
    let _ = std::fs::create_dir_all(&data_dir);
    Hello {
        api: API_VERSION,
        host: HostInfo { name: "test".into(), version: "0".into() },
        config,
        systems: vec![SystemInfo { index: 0, short_name: "sys1".into(), kind: "p25".into(), config: Value::Null }],
        capture_dir: dir.to_path_buf(),
        data_dir,
        audio_formats: vec!["wav".into(), format::M4A.into()],
    }
}

/// A 3-second call on talkgroup `tg` of `short_name` (system 0), with its
/// files written under `dir`: the JSON, a WAV of silence, and an "M4A"
/// (placeholder bytes, not real audio) in `files.m4a`.
pub fn call(dir: &Path, short_name: &str, tg: u32) -> ConcludedCall {
    let start = 1_700_000_000 + tg as i64;
    let path = format!("{short_name}/2023/11/14/{tg}-{start}_851012500");
    let base = dir.join(&path);
    std::fs::create_dir_all(base.parent().unwrap()).expect("call dir");
    let record = json!({
        "call_num": tg, "short_name": short_name, "talkgroup": tg, "talkgroup_tag": format!("TG {tg}"),
        "freq": 851012500u64, "start_time": start, "stop_time": start + 3, "call_length": 3,
        "emergency": 0, "encrypted": 0, "audio_type": "digital",
        "freqList": [{ "freq": 851012500u64, "time": start, "pos": 0, "len": 3, "error_count": 2, "spike_count": 0 }],
        "srcList": [{ "src": 1234, "time": start, "pos": 0, "emergency": 0, "signal_system": "", "tag": "", "tag_ota": "" }],
    });
    let files = CallFiles { json: ext(&base, "json"), wav: ext(&base, "wav"), m4a: Some(ext(&base, "m4a")) };
    std::fs::write(&files.json, record.to_string()).expect("json");
    std::fs::write(&files.wav, silent_wav(8000, 3)).expect("wav");
    std::fs::write(files.m4a.as_ref().unwrap(), b"\0\0\0\x18ftypM4A test").expect("m4a");
    ConcludedCall { path, system: 0, call: serde_json::from_value(record).expect("record"), files }
}

fn ext(base: &Path, e: &str) -> PathBuf {
    PathBuf::from(format!("{}.{e}", base.display()))
}

fn silent_wav(rate: u32, secs: u32) -> Vec<u8> {
    let data = rate * secs * 2;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    w.resize(44 + data as usize, 0);
    w
}

/// A request [`MockServer`] got.
#[derive(Clone, Debug, Default)]
pub struct Request {
    pub method: String,
    /// The path and query.
    pub path: String,
    /// Header names in lowercase.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str())
    }

    /// A `multipart/form-data` field's value (a file field's bytes).
    pub fn form_field(&self, name: &str) -> Option<Vec<u8>> {
        self.form_part(name).map(|(_, v)| v)
    }

    /// A `multipart/form-data` file field's file name.
    pub fn form_file_name(&self, name: &str) -> Option<String> {
        let (headers, _) = self.form_part(name)?;
        let i = headers.find("filename=\"")? + 10;
        Some(headers[i..].split('"').next()?.to_string())
    }

    fn form_part(&self, name: &str) -> Option<(String, Vec<u8>)> {
        let ct = self.header("content-type")?;
        let boundary = format!("--{}", ct.split("boundary=").nth(1)?.trim_matches('"'));
        let b = &self.body;
        let mut at = find(b, boundary.as_bytes(), 0)?;
        loop {
            let start = at + boundary.len() + 2;
            let next = find(b, boundary.as_bytes(), start)?;
            let part = &b[start..next.saturating_sub(2)];
            let split = find(part, b"\r\n\r\n", 0)?;
            let headers = String::from_utf8_lossy(&part[..split]).to_string();
            if headers.contains(&format!("name=\"{name}\"")) {
                return Some((headers, part[split + 4..].to_vec()));
            }
            at = next;
        }
    }
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

type Respond = dyn Fn(&Request) -> (u16, String) + Send + Sync;

/// A web server on 127.0.0.1 for testing a plugin that talks to one: it
/// answers every request with `respond` and keeps them for you to look at.
///
/// ```no_run
/// # use trunk_recorder_plugin::testing::MockServer;
/// let server = MockServer::start(|req| if req.path.ends_with("/upload") { (200, String::new()) } else { (404, "no".into()) });
/// // … point the plugin at server.url() …
/// assert_eq!(server.requests().len(), 1);
/// ```
pub struct MockServer {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl MockServer {
    pub fn start(respond: impl Fn(&Request) -> (u16, String) + Send + Sync + 'static) -> MockServer {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (reqs, respond): (_, Arc<Respond>) = (requests.clone(), Arc::new(respond));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (reqs, respond) = (reqs.clone(), respond.clone());
                std::thread::spawn(move || serve_one(stream, &reqs, &*respond));
            }
        });
        MockServer { url, requests }
    }

    /// `http://127.0.0.1:<port>`
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The requests so far.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

fn serve_one(mut s: std::net::TcpStream, reqs: &Mutex<Vec<Request>>, respond: &Respond) {
    use std::io::Read;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16384];
    // Headers, then Content-Length bytes of body (keep-alive: one request per connection is plenty).
    let head_end = loop {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if let Some(i) = find(&buf, b"\r\n\r\n", 0) {
            break i;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, path) = (first.next().unwrap_or("").to_string(), first.next().unwrap_or("").to_string());
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())).collect();
    let len: usize = headers.iter().find(|(k, _)| k == "content-length").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    let req = Request { method, path, headers, body };
    let (status, text) = respond(&req);
    reqs.lock().unwrap().push(req);
    let resp = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len());
    let _ = s.write_all(resp.as_bytes());
}
