//! The recorder's own web API, read-only: each radio's figures, the stats
//! history (drops, clipping, control channels, the computer, the clock, the
//! other plugins). A plugin's protocol doesn't carry these, so the plugin
//! reads them where the recorder's page does: its web socket on this computer.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// One series of the history: per-step average / lowest / highest.
#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub struct Series {
    pub step_s: f64,
    pub v: Vec<Option<f64>>,
    pub lo: Vec<Option<f64>>,
    pub hi: Vec<Option<f64>>,
}

impl Series {
    fn from(j: &Value) -> Series {
        let arr = |k: &str| j[k].as_array().map(|a| a.iter().map(|x| x.as_f64()).collect()).unwrap_or_default();
        Series { step_s: j["stepS"].as_f64().unwrap_or(60.0), v: arr("v"), lo: arr("lo"), hi: arr("hi") }
    }
    pub fn vals(&self) -> Vec<f64> {
        self.v.iter().flatten().copied().collect()
    }
    pub fn his(&self) -> Vec<f64> {
        self.hi.iter().flatten().copied().collect()
    }
    pub fn last(&self) -> Option<f64> {
        self.v.iter().rev().flatten().next().copied()
    }
    /// A counter's total over the span (its values are per second).
    pub fn total(&self) -> f64 {
        self.v.iter().flatten().map(|x| x * self.step_s).sum()
    }
    /// The total over the last `secs`.
    pub fn total_last(&self, secs: f64) -> f64 {
        let n = ((secs / self.step_s.max(1.0)).ceil() as usize).max(1);
        self.v.iter().rev().take(n).flatten().map(|x| x * self.step_s).sum()
    }
}

/// What the recorder said.
#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct Live {
    pub version: String,
    pub port: u16,
    /// The latest `status` message.
    pub status: Value,
    pub series: BTreeMap<String, Series>,
}

/// The recorder's port: the setting, else its config file's, else the first that answers.
pub fn find_port(setting: u16, data_dir: &Path) -> Option<u16> {
    if setting != 0 {
        return Some(setting);
    }
    // data_dir is <config folder>/plugin-data/<id>; config.json sits in the config folder.
    if let Some(dir) = data_dir.parent().and_then(|p| p.parent()) {
        if let Ok(s) = std::fs::read_to_string(dir.join("config.json")) {
            if let Ok(v) = serde_json::from_str::<Value>(&s) {
                if let Some(p) = v["server"]["port"].as_u64() {
                    return Some(p as u16);
                }
            }
        }
    }
    (8080..8100).find(|p| WebSocket::open(*p, Duration::from_millis(800)).is_ok())
}

/// Read the recorder's figures for the last `window_s`.
pub fn gather(port: u16, window_s: u64) -> Result<Live, String> {
    let mut ws = WebSocket::open(port, Duration::from_secs(5)).map_err(|e| format!("can't reach the recorder's web API on port {port}: {e}"))?;
    let mut live = Live { port, ..Default::default() };
    let mut statuses = 0;
    let mut got_stats = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let m = ws.recv().map_err(|e| format!("the recorder's web API: {e}"))?;
        match m["type"].as_str().unwrap_or("") {
            "hello" => {
                live.version = m["version"].as_str().unwrap_or("").to_string();
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
                ws.send(&json!({ "type": "statsQuery", "id": "health", "series": ["src/*", "sys/*", "plg/*", "plat/*", "clock/*"],
                                 "from": now - window_s as i64, "to": now, "points": 120 }))
                    .map_err(|e| format!("the recorder's web API: {e}"))?;
            }
            "status" => {
                live.status = m;
                statuses += 1;
            }
            "statsResult" if m["id"] == "health" => {
                if let Some(o) = m["series"].as_object() {
                    live.series = o.iter().map(|(k, v)| (k.clone(), Series::from(v))).collect();
                }
                got_stats = true;
            }
            _ => {}
        }
        if got_stats && statuses >= 1 {
            break;
        }
    }
    if !got_stats {
        return Err("the recorder's web API didn't answer with its figures".into());
    }
    Ok(live)
}

/// Just enough of a web socket client (RFC 6455) for one local, read-only conversation.
struct WebSocket {
    s: TcpStream,
    buf: Vec<u8>,
}

impl WebSocket {
    fn open(port: u16, timeout: Duration) -> std::io::Result<WebSocket> {
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        let mut s = TcpStream::connect_timeout(&addr, timeout)?;
        s.set_read_timeout(Some(Duration::from_secs(10)))?;
        s.set_write_timeout(Some(Duration::from_secs(5)))?;
        let key = "dHJwLWhlYWx0aC1wbHVnaW4="; // any 16 bytes, base64
        write!(s, "GET /api/ws HTTP/1.1\r\nHost: localhost:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n")?;
        let mut head = Vec::new();
        let mut b = [0u8; 4096];
        let end = loop {
            let n = s.read(&mut b)?;
            if n == 0 {
                return Err(std::io::Error::other("closed during the handshake"));
            }
            head.extend_from_slice(&b[..n]);
            if let Some(i) = head.windows(4).position(|w| w == b"\r\n\r\n") {
                break i;
            }
            if head.len() > 65536 {
                return Err(std::io::Error::other("not a web socket"));
            }
        };
        let first = String::from_utf8_lossy(&head[..head.iter().position(|&c| c == b'\r').unwrap_or(end)]).to_string();
        if !first.contains(" 101 ") {
            return Err(std::io::Error::other(first));
        }
        Ok(WebSocket { s, buf: head[end + 4..].to_vec() })
    }

    fn need(&mut self, n: usize) -> std::io::Result<Vec<u8>> {
        let mut b = [0u8; 65536];
        while self.buf.len() < n {
            let k = self.s.read(&mut b)?;
            if k == 0 {
                return Err(std::io::Error::other("connection closed"));
            }
            self.buf.extend_from_slice(&b[..k]);
        }
        Ok(self.buf.drain(..n).collect())
    }

    fn frame(&mut self, op: u8, data: &[u8]) -> std::io::Result<()> {
        let mut f = vec![0x80 | op];
        let n = data.len();
        if n < 126 {
            f.push(0x80 | n as u8);
        } else if n < 65536 {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
        let mask = [0x5a, 0x17, 0xc3, 0x2e];
        f.extend_from_slice(&mask);
        f.extend(data.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.s.write_all(&f)
    }

    fn send(&mut self, v: &Value) -> std::io::Result<()> {
        self.frame(0x1, v.to_string().as_bytes())
    }

    /// The next whole text message, parsed (one that isn't JSON is skipped).
    fn recv(&mut self) -> std::io::Result<Value> {
        let mut parts = Vec::new();
        loop {
            let h = self.need(2)?;
            let (fin, op, masked, mut n) = (h[0] & 0x80 != 0, h[0] & 0x0f, h[1] & 0x80 != 0, (h[1] & 0x7f) as usize);
            if n == 126 {
                let b = self.need(2)?;
                n = u16::from_be_bytes([b[0], b[1]]) as usize;
            } else if n == 127 {
                let b = self.need(8)?;
                n = u64::from_be_bytes(b.try_into().unwrap_or([0; 8])) as usize;
            }
            let mask = if masked { Some(self.need(4)?) } else { None };
            let mut data = self.need(n)?;
            if let Some(m) = mask {
                for (i, b) in data.iter_mut().enumerate() {
                    *b ^= m[i % 4];
                }
            }
            match op {
                0x9 => self.frame(0xA, &data)?,
                0x8 => return Err(std::io::Error::other("closed by the recorder")),
                0x0..=0x2 => {
                    parts.extend_from_slice(&data);
                    if fin {
                        match serde_json::from_slice::<Value>(&parts) {
                            Ok(v) => return Ok(v),
                            Err(_) => parts.clear(),
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

impl Drop for WebSocket {
    fn drop(&mut self) {
        let _ = self.frame(0x8, &[]);
    }
}
