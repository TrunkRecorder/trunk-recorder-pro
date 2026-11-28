//! A receiver on another machine, through Airspy's `spyserver` (an Airspy R2 / Mini / HF+, or an RTL-SDR
//! behind it): plain TCP, nothing to install.
//!
//! The protocol has no specification: it is `spyserver_protocol.h` in SDR#'s and SDR++'s sources, and what
//! the servers do. Everything is little-endian. A command is `{type, body size}` and its body; a message
//! from the server is `{protocol id, type, stream type, sequence, body size}` and its body, the type's
//! high half being, on samples, the digital gain the server applied in whole dB.
//!
//! One stream is used: I/Q only, at one of the server's decimation stages (its top rate halved so many
//! times), so the channelizer gets the receiver's own samples. Whatever gain the server applied is divided
//! out again, so levels are the same whatever it was set to. The server's FFT stream isn't used.
//!
//! A server lets any number of clients listen but only one move the receiver: a source that isn't the first
//! to connect can only slide its window inside what the first has tuned, and says so.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use num_complex::Complex32;
use trunk_app::config::SampleFormat;

use super::net::{timed_out, CONNECT, POLL, SILENT};
use super::{Caps, Deadline, Rates};
use crate::sdr::{Applied, Control, Gain, Radio, Regained, Retuned, Silence, Sink, Tunable};

#[derive(Clone, Debug)]
pub struct SpyserverConfig {
    /// The server, `host:port` ([`trunk_app::config::Source::server`]).
    pub server: String,
    pub center_hz: f64,
    pub rate_hz: f64,
    pub format: SampleFormat,
    /// The server's gain step (held to what it has).
    pub gain: u32,
    /// What the server multiplies by before quantising; None: the reference clients' rule.
    pub digital_gain_db: Option<u8>,
}

impl SpyserverConfig {
    fn name(&self) -> String {
        format!("SpyServer {}", self.server)
    }
}

/// A ping this often keeps a server from taking a quiet client for a dead one.
const PING_EVERY: Duration = Duration::from_secs(3);
/// The floor between retunes: more are more than a server will do.
const RETUNE_EVERY: Duration = Duration::from_millis(125);

// --- the protocol ------------------------------------------------------------------------------

/// `{u32 command, u32 body size}`.
const CMD_HEADER: usize = 8;
/// `{u32 protocol id, u32 message type, u32 stream type, u32 sequence, u32 body size}`.
const MSG_HEADER: usize = 20;
/// Past this a body is a stream out of step, and nothing in it says where the next header is.
const MAX_BODY: u32 = 1 << 20;
/// What this announces, as the wire packs it (major, minor, build): servers check the major.
const PROTOCOL_VERSION: u32 = (2 << 24) | 1700;
const PROTOCOL_MAJOR: u32 = 2;

const CMD_HELLO: u32 = 0;
const CMD_SET_SETTING: u32 = 2;
const CMD_PING: u32 = 3;

const SETTING_STREAMING_MODE: u32 = 0;
const SETTING_STREAMING_ENABLED: u32 = 1;
const SETTING_GAIN: u32 = 2;
const SETTING_IQ_FORMAT: u32 = 100;
const SETTING_IQ_FREQUENCY: u32 = 101;
const SETTING_IQ_DECIMATION: u32 = 102;
const SETTING_IQ_DIGITAL_GAIN: u32 = 103;

const STREAM_MODE_IQ_ONLY: u32 = 1;

const FORMAT_UINT8: u32 = 1;
const FORMAT_INT16: u32 = 2;
const FORMAT_INT24: u32 = 3;
const FORMAT_FLOAT: u32 = 4;

const MSG_DEVICE_INFO: u16 = 0;
const MSG_CLIENT_SYNC: u16 = 1;
const MSG_UINT8_IQ: u16 = 100;
const MSG_INT16_IQ: u16 = 101;
const MSG_FLOAT_IQ: u16 = 103;

const DEVICE_INFO_LEN: usize = 12 * 4;
const CLIENT_SYNC_LEN: usize = 9 * 4;

/// The most digital gain asked for.
const DIGITAL_GAIN_MAX: f64 = 60.0;

fn frame_hello(who: &str) -> Vec<u8> {
    let body = 4 + who.len();
    let mut out = Vec::with_capacity(CMD_HEADER + body);
    out.extend_from_slice(&CMD_HELLO.to_le_bytes());
    out.extend_from_slice(&(body as u32).to_le_bytes());
    out.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    out.extend_from_slice(who.as_bytes());
    out
}

fn frame_setting(setting: u32, value: u32) -> [u8; CMD_HEADER + 8] {
    let mut out = [0u8; CMD_HEADER + 8];
    out[0..4].copy_from_slice(&CMD_SET_SETTING.to_le_bytes());
    out[4..8].copy_from_slice(&8u32.to_le_bytes());
    out[8..12].copy_from_slice(&setting.to_le_bytes());
    out[12..16].copy_from_slice(&value.to_le_bytes());
    out
}

fn frame_ping() -> [u8; CMD_HEADER] {
    let mut out = [0u8; CMD_HEADER];
    out[0..4].copy_from_slice(&CMD_PING.to_le_bytes());
    out
}

/// A frequency as the protocol carries it: whole hertz, held to what it can say (a wrapped value would
/// tune somewhere near 4 GHz).
fn freq_wire(hz: f64) -> u32 {
    hz.round().clamp(0.0, f64::from(u32::MAX)) as u32
}

fn format_wire(f: SampleFormat) -> u32 {
    match f {
        SampleFormat::Cu8 => FORMAT_UINT8,
        SampleFormat::Cs16 => FORMAT_INT16,
        SampleFormat::Cf32 => FORMAT_FLOAT,
    }
}

/// The format a server insists on (`forced_iq_format`; 0 = none), if there is one this reads.
fn forced_format(code: u32) -> Result<Option<SampleFormat>, String> {
    match code {
        0 => Ok(None),
        FORMAT_UINT8 => Ok(Some(SampleFormat::Cu8)),
        FORMAT_INT16 => Ok(Some(SampleFormat::Cs16)),
        FORMAT_FLOAT => Ok(Some(SampleFormat::Cf32)),
        FORMAT_INT24 => Err("it forces 24-bit I/Q, which isn't read here".into()),
        v => Err(format!("it forces I/Q format {v}, which isn't read here")),
    }
}

/// A message's header.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Header {
    kind: u16,
    /// On samples: the digital gain the server applied, whole dB.
    flags: u16,
    sequence: u32,
    body: usize,
}

/// `b` must hold [`MSG_HEADER`] bytes. Both refusals are fatal: with no marker to find the next header by,
/// a stream out of step stays out of step.
fn parse_header(b: &[u8]) -> Result<Header, String> {
    let w = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let id = w(0);
    if id >> 24 != PROTOCOL_MAJOR {
        return Err(format!("it announced protocol {}.{}.{}, and this reads {PROTOCOL_MAJOR}.x: is that a SpyServer?", id >> 24, (id >> 16) & 0xFF, id & 0xFFFF));
    }
    let body = w(16);
    if body > MAX_BODY {
        return Err(format!("it announced a {body}-byte message, past the protocol's {MAX_BODY}: the stream is out of step"));
    }
    let t = w(4);
    Ok(Header { kind: (t & 0xFFFF) as u16, flags: (t >> 16) as u16, sequence: w(12), body: body as usize })
}

/// The first whole message of `b`: its header and the bytes it takes, or None while the rest is to come.
fn next_message(b: &[u8]) -> Result<Option<(Header, usize)>, String> {
    if b.len() < MSG_HEADER {
        return Ok(None);
    }
    let h = parse_header(b)?;
    let total = MSG_HEADER + h.body;
    Ok((b.len() >= total).then_some((h, total)))
}

/// What the server says of the receiver behind it (the fields used here).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Info {
    device: u32,
    serial: u32,
    max_rate: u32,
    max_bandwidth: u32,
    stages: u32,
    max_gain: u32,
    min_hz: u32,
    max_hz: u32,
    resolution: u32,
    min_decimation: u32,
    forced_format: u32,
}

fn parse_info(b: &[u8]) -> Result<Info, String> {
    if b.len() < DEVICE_INFO_LEN {
        return Err(format!("its device-info message is {} bytes; it should be {DEVICE_INFO_LEN}", b.len()));
    }
    let w = |i: usize| u32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]);
    Ok(Info {
        device: w(0),
        serial: w(1),
        max_rate: w(2),
        max_bandwidth: w(3),
        stages: w(4),
        max_gain: w(6),
        min_hz: w(7),
        max_hz: w(8),
        resolution: w(9),
        min_decimation: w(10),
        forced_format: w(11),
    })
}

impl Info {
    fn model(&self) -> String {
        match self.device {
            0 => "no receiver".into(),
            1 => "Airspy R2 / Mini".into(),
            2 => "Airspy HF+".into(),
            3 => "RTL-SDR".into(),
            v => format!("device type {v}"),
        }
    }

    /// The I/Q rates it offers, as (decimation stage, Hz), widest first: its top rate halved so many times,
    /// from the stage it starts at, each the nearest whole number of Hz to that quotient (a half rounds up).
    fn rates(&self) -> Vec<(u32, f64)> {
        (self.min_decimation..=self.stages)
            .map(|i| {
                let div = 1u64 << i.min(31);
                (i, ((u64::from(self.max_rate) + div / 2) / div) as f64)
            })
            .collect()
    }

    /// The digital gain to start from, dB: the reference clients' rule. Each decimation stage halves the
    /// bandwidth and is worth about 3 dB that would be lost when the samples are quantised for the link,
    /// and an Airspy R2's lower gain steps leave a quieter signal that needs the headroom back.
    fn digital_gain_db(&self, gain_index: u32, stage: u32) -> f64 {
        let stage_gain = f64::from(stage) * 3.01;
        if self.device == 1 {
            f64::from(self.max_gain.saturating_sub(gain_index)) + stage_gain
        } else {
            stage_gain
        }
    }

    /// How far a window of `rate_hz` may slide either side of the receiver's centre without the receiver
    /// moving: against the analog passband, not the sample rate (what's between is roll-off).
    fn slack_hz(&self, rate_hz: f64) -> f64 {
        let rate = f64::from(self.max_rate);
        let usable = if self.max_bandwidth > 0 { f64::from(self.max_bandwidth).min(rate) } else { rate };
        (usable - rate_hz).max(0.0) / 2.0
    }

    /// The rates it offers, whole Hz, widest first (a stream under 100 kHz is no use to a recorder, so none).
    fn offered(&self) -> Vec<u32> {
        self.rates().iter().map(|&(_, r)| r as u32).filter(|&r| r >= 100_000).collect()
    }

    fn describe(&self) -> String {
        format!(
            "{} (serial {:08X}), {:.3} MSPS max, {}-bit, gain 0..{}, {:.3}–{:.3} MHz",
            self.model(),
            self.serial,
            f64::from(self.max_rate) / 1e6,
            self.resolution,
            self.max_gain,
            f64::from(self.min_hz) / 1e6,
            f64::from(self.max_hz) / 1e6
        )
    }
}

/// Where the server says things are now (it resends this when they change).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct ClientSync {
    /// This client may move the receiver (it was first), not just its window in what another tuned.
    can_control: bool,
    gain: u32,
    device_center_hz: f64,
}

fn parse_sync(b: &[u8]) -> Result<ClientSync, String> {
    if b.len() < CLIENT_SYNC_LEN {
        return Err(format!("its client-sync message is {} bytes; it should be {CLIENT_SYNC_LEN}", b.len()));
    }
    let w = |i: usize| u32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]);
    Ok(ClientSync { can_control: w(0) != 0, gain: w(1), device_center_hz: f64::from(w(2)) })
}

/// A sample message as complex floats in `out` (full scale ±1), with the gain the server applied divided
/// out (`gain`: linear).
fn decode(kind: u16, body: &[u8], gain: f32, out: &mut Vec<Complex32>) -> Result<(), String> {
    let format = match kind {
        MSG_UINT8_IQ => SampleFormat::Cu8,
        MSG_INT16_IQ => SampleFormat::Cs16,
        MSG_FLOAT_IQ => SampleFormat::Cf32,
        other => return Err(format!("a sample message of type {other}, which isn't read here")),
    };
    // (As a capture file's samples are read, the gain the server applied divided out.)
    let k = 1.0 / gain;
    out.clear();
    out.extend(trunk_app::samples::to_iq(format, body).into_iter().map(|c| c * k));
    Ok(())
}

// --- the connection ----------------------------------------------------------------------------

/// A connected, introduced server.
struct Client {
    sock: TcpStream,
    name: String,
    info: Info,
    can_control: bool,
    device_center: f64,
    /// The decimation stage, and the rate it makes.
    stage: u32,
    rate: f64,
    format: SampleFormat,
    gain_index: u32,
    /// The digital gain the source sets, dB; none: it follows from the gain step.
    digital_set: Option<u8>,
    /// Where the window is.
    center: f64,
    last_retune: Instant,
}

impl Client {
    fn set(&mut self, setting: u32, value: u32) -> Result<(), String> {
        self.sock.write_all(&frame_setting(setting, value)).map_err(|e| format!("{}: lost the connection while sending: {e}", self.name))
    }

    fn ping(&mut self) -> Result<(), String> {
        self.sock.write_all(&frame_ping()).map_err(|e| format!("{}: lost the connection while sending: {e}", self.name))
    }

    /// Where the window may sit: anywhere the receiver tunes if this client moves it, else the slack either
    /// side of where another client has it.
    fn reachable(&self, want: f64) -> f64 {
        let (lo, hi) = if self.can_control {
            (f64::from(self.info.min_hz), f64::from(self.info.max_hz))
        } else {
            let slack = self.info.slack_hz(self.rate);
            (self.device_center - slack, self.device_center + slack)
        };
        // (A server that leaves its range at zero has no range to hold to.)
        if hi <= lo && self.can_control {
            want
        } else {
            want.clamp(lo, hi.max(lo))
        }
    }

    fn adopt(&mut self, s: &ClientSync) {
        self.can_control = s.can_control;
        self.device_center = s.device_center_hz;
        if !self.can_control {
            // (The gain is the other client's then.)
            self.gain_index = s.gain;
        }
    }

    /// The digital gain to ask for, whole dB.
    fn digital_gain(&self, set: Option<u8>) -> u32 {
        let db = set.map_or_else(|| self.info.digital_gain_db(self.gain_index, self.stage), f64::from);
        db.round().clamp(0.0, DIGITAL_GAIN_MAX) as u32
    }

    /// Ask the server to stop before the socket goes, so it can free the receiver for the next client.
    fn shutdown(&mut self) {
        let _ = self.set(SETTING_STREAMING_ENABLED, 0);
        let _ = self.sock.shutdown(Shutdown::Both);
    }
}

impl Tunable for Client {
    fn tune(&mut self, hz: f64) -> Result<Retuned, String> {
        // (Past this a server spends its time retuning: the wait is short, and a survey's steps are longer.)
        let since = self.last_retune.elapsed();
        if since < RETUNE_EVERY {
            std::thread::sleep(RETUNE_EVERY - since);
        }
        let sent = self.reachable(hz);
        self.set(SETTING_IQ_FREQUENCY, freq_wire(sent))?;
        self.last_retune = Instant::now();
        self.center = sent;
        // (It keeps streaming through a retune, and says nothing back but its next sync: the centre is where it
        // was put, held to what it reaches.)
        Ok(Retuned { hz: sent, gap: 0 })
    }
    fn set_gain(&mut self, g: Gain) -> Result<Regained, String> {
        // (A server's gain is a step of its own; only a step is taken, and only by the client that has the receiver:
        // another's gain stands.)
        let Gain::Step(n) = g else { return Ok(Regained::LEFT_ALONE) };
        if !self.can_control {
            return Ok(Regained::LEFT_ALONE);
        }
        let n = n.min(self.info.max_gain);
        self.set(SETTING_GAIN, n)?;
        self.gain_index = n;
        // (The digital gain follows the step, as at the start; the samples say what was applied, so the level is kept.)
        let dg = self.digital_gain(self.digital_set);
        self.set(SETTING_IQ_DIGITAL_GAIN, dg)?;
        Ok(Regained { gap: 0, gain: Some(Gain::Step(n)) })
    }
}

/// Say hello and read the server's device info and sync: `(info, sync, bytes already read beyond them)`.
fn handshake(sock: &mut TcpStream, name: &str) -> Result<(Info, ClientSync, Vec<u8>), String> {
    sock.write_all(&frame_hello("Trunk Recorder Pro")).map_err(|e| format!("{name}: {e}"))?;
    let until = Instant::now() + CONNECT;
    let mut pending: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    let (mut info, mut sync) = (None, None);
    loop {
        let mut at = 0;
        while let Some((h, total)) = next_message(&pending[at..]).map_err(|e| format!("{name}: {e}"))? {
            let body = &pending[at + MSG_HEADER..at + total];
            match h.kind {
                MSG_DEVICE_INFO => info = Some(parse_info(body).map_err(|e| format!("{name}: {e}"))?),
                MSG_CLIENT_SYNC => sync = Some(parse_sync(body).map_err(|e| format!("{name}: {e}"))?),
                _ => {}
            }
            at += total;
        }
        pending.drain(..at);
        if let (Some(i), Some(s)) = (info, sync) {
            return Ok((i, s, pending));
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!("{name}: it connected but didn't say what it is: is that a SpyServer?"));
        }
        sock.set_read_timeout(Some(left)).map_err(|e| format!("{name}: {e}"))?;
        match sock.read(&mut buf) {
            Ok(0) => return Err(format!("{name}: it hung up at hello (is that a SpyServer? One with no client slot free does)")),
            Ok(n) => pending.extend_from_slice(&buf[..n]),
            Err(e) if timed_out(&e) => {}
            Err(e) => return Err(format!("{name}: {e}")),
        }
    }
}

/// What the SpyServer at `server` (`host:port`) is remembered by ([`super::remember`]).
pub fn key(server: &str) -> String {
    super::net::key("spyserver", server)
}

impl Info {
    /// What it said of the receiver behind it, as a radio's [`Caps`]: its model, the rates it offers, its gain steps (0 up).
    fn caps(&self) -> Caps {
        Caps { model: self.model(), rates: Rates::List(self.offered()), gains: trunk_app::gain::Reported { steps: (0..=self.max_gain).map(f64::from).collect(), ..Default::default() } }
    }
}

/// Connect to the server at `server` (`host:port`, as `name`), and read its hello: the receiver behind it (remembered, as any
/// radio is when opened), where it's tuned, and bytes already read beyond them.
fn hello(name: &str, server: &str) -> Result<(TcpStream, Info, ClientSync, Vec<u8>), String> {
    let mut sock = super::net::dial(name, server, CONNECT)?;
    let (info, sync, pending) = handshake(&mut sock, name)?;
    if info.device == 0 {
        return Err(format!("{name}: the server has no receiver attached"));
    }
    super::remember(&key(server), info.caps());
    Ok((sock, info, sync, pending))
}

/// What the receiver behind the SpyServer at `server` (`host:port`) is, for a list of radios: connect, read the
/// hello, hang up. Nothing is set, so a receiver another client is using is left as it is.
pub fn ask(server: &str) -> Result<Caps, String> {
    hello(&format!("SpyServer {server}"), server).map(|(_, info, ..)| info.caps())
}

/// Connect, introduce, and settle what the stream will be (the stage, the format, where the window can sit).
fn connect(cfg: &SpyserverConfig) -> Result<(Client, Vec<u8>), String> {
    let name = cfg.name();
    let (sock, info, sync, pending) = hello(&name, &cfg.server)?;
    let rates = info.rates();
    let Some(&(stage, rate)) = rates.iter().find(|&&(_, r)| (r - cfg.rate_hz).abs() < 1.0) else {
        return Err(format!("{name}: {}", super::rate_refusal(&format!("SpyServer ({})", info.model()), &Rates::List(info.offered()), cfg.rate_hz)));
    };
    let format = match forced_format(info.forced_format).map_err(|e| format!("{name}: {e}"))? {
        Some(f) => {
            if f != cfg.format {
                log::info!("{name}: it sends {} I/Q whatever is asked", f.describe());
            }
            f
        }
        None => cfg.format,
    };
    let mut client = Client {
        sock,
        name: name.clone(),
        info,
        can_control: sync.can_control,
        device_center: sync.device_center_hz,
        stage,
        rate,
        format,
        gain_index: cfg.gain.min(info.max_gain),
        digital_set: cfg.digital_gain_db,
        center: cfg.center_hz,
        last_retune: Instant::now().checked_sub(RETUNE_EVERY).unwrap_or_else(Instant::now),
    };
    if !client.can_control {
        client.gain_index = sync.gain;
        let slack = info.slack_hz(rate);
        if (cfg.center_hz - client.device_center).abs() > slack + 1000.0 {
            return Err(format!(
                "{name}: another client has the receiver, on {:.3} MHz, and a second can only move its window within ±{:.0} kHz of that: {:.3} MHz is out of reach until that client lets go",
                client.device_center / 1e6,
                slack / 1e3,
                cfg.center_hz / 1e6
            ));
        }
    }
    Ok((client, pending))
}

/// What a connection is told before it streams. The order is the protocol's: the mode before the gain (a server
/// may reset its gain when the mode changes), and the digital gain after, as it follows from the gain step.
fn start(c: &mut Client, cfg: &SpyserverConfig) -> Result<(), String> {
    c.set(SETTING_IQ_FORMAT, format_wire(c.format))?;
    c.set(SETTING_IQ_DECIMATION, c.stage)?;
    let at = c.reachable(cfg.center_hz);
    c.center = at;
    c.set(SETTING_IQ_FREQUENCY, freq_wire(at))?;
    c.set(SETTING_STREAMING_MODE, STREAM_MODE_IQ_ONLY)?;
    if c.can_control {
        c.set(SETTING_GAIN, c.gain_index)?;
    }
    let dg = c.digital_gain(cfg.digital_gain_db);
    c.set(SETTING_IQ_DIGITAL_GAIN, dg)?;
    c.set(SETTING_STREAMING_ENABLED, 1)?;
    c.last_retune = Instant::now();
    let link_mbit = c.rate * c.format.bytes_per_sample() as f64 * 8.0 / 1e6;
    log::info!(
        "{}: {}, {:.3} MSPS {} ({link_mbit:.1} Mbit/s), gain step {}, digital gain {dg} dB{}",
        c.name,
        c.info.describe(),
        c.rate / 1e6,
        c.format.describe(),
        c.gain_index,
        if c.can_control { "" } else { " (another client has the receiver: its tuning and gain stand)" }
    );
    Ok(())
}

impl Radio for SpyserverConfig {
    const BACKOFF: Duration = Duration::from_secs(3);
    fn rate_hz(&self) -> f64 {
        self.rate_hz
    }
    fn retune_to(&mut self, hz: f64) {
        self.center_hz = hz;
    }
    fn gain_to(&mut self, g: Gain) {
        if let Gain::Step(n) = g {
            self.gain = n;
        }
    }
    fn gain(&self) -> Option<Gain> {
        Some(Gain::Step(self.gain))
    }
    fn stream_once(&self, sink: &mut Sink, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
        stream_once(self, sink, stop, ctl)
    }
}

fn stream_once(cfg: &SpyserverConfig, sink: &mut Sink, stop: &AtomicBool, ctl: Option<&Control>) -> Result<(), String> {
    let name = cfg.name();
    let (mut client, pending) = {
        let cfg = cfg.clone();
        super::open(&name, Deadline::Loads, false, move || connect(&cfg))?
    };
    sink.opened(format!("{:08X}", client.info.serial), client.info.model());
    let result = start(&mut client, cfg).and_then(|()| run(&mut client, pending, sink, stop, ctl, cfg.center_hz));
    client.shutdown();
    result
}

fn run(c: &mut Client, mut pending: Vec<u8>, sink: &mut Sink, stop: &AtomicBool, ctl: Option<&Control>, center_hz: f64) -> Result<(), String> {
    c.sock.set_read_timeout(Some(POLL)).map_err(|e| format!("{}: {e}", c.name))?;
    let mut buf = vec![0u8; 64 << 10];
    let mut samples: Vec<Complex32> = Vec::new();
    let mut silence = Silence::new(SILENT);
    let mut pinged = Instant::now();
    // Messages the server skipped (it does when a client falls behind) show as a gap in the sequence.
    let mut last_seq: Option<u32> = None;
    let mut last_len = 0u64;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Some(ctl) = ctl {
            match ctl.apply(c, center_hz, sink) {
                Err(e) => return Err(format!("{}: {e}", c.name)),
                Ok(Applied::Closed) => return Ok(()),
                Ok(_) => {}
            }
        }
        if pinged.elapsed() >= PING_EVERY {
            pinged = Instant::now();
            c.ping()?;
        }
        match c.sock.read(&mut buf) {
            Ok(0) => return Err(format!("{}: the server closed the connection (another client may have taken the receiver, or it lost it)", c.name)),
            Ok(n) => {
                silence.heard(Instant::now());
                pending.extend_from_slice(&buf[..n]);
            }
            Err(e) if timed_out(&e) => silence.check(&c.name, Instant::now())?,
            Err(e) => return Err(format!("{}: {e}", c.name)),
        }
        let mut at = 0;
        while let Some((h, total)) = next_message(&pending[at..]).map_err(|e| format!("{}: {e}", c.name))? {
            let body = &pending[at + MSG_HEADER..at + total];
            at += total;
            match h.kind {
                MSG_UINT8_IQ | MSG_INT16_IQ | MSG_FLOAT_IQ => {
                    decode(h.kind, body, 10f32.powf(f32::from(h.flags) / 20.0), &mut samples).map_err(|e| format!("{}: {e}", c.name))?;
                    let mut dropped = 0;
                    if let Some(prev) = last_seq {
                        let gap = h.sequence.wrapping_sub(prev).wrapping_sub(1);
                        // (A big jump is a counter that started over, not that many lost messages.)
                        if gap > 0 && gap < 1000 {
                            dropped = u64::from(gap) * last_len;
                        }
                    }
                    last_seq = Some(h.sequence);
                    last_len = samples.len() as u64;
                    if !sink.iq(&samples, dropped, silence.last()) {
                        return Ok(());
                    }
                }
                MSG_CLIENT_SYNC => {
                    if let Ok(s) = parse_sync(body) {
                        c.adopt(&s);
                    }
                }
                _ => {}
            }
        }
        pending.drain(..at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn message(kind: u16, flags: u16, sequence: u32, body: &[u8]) -> Vec<u8> {
        let mut m = Vec::new();
        m.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        m.extend_from_slice(&(u32::from(kind) | (u32::from(flags) << 16)).to_le_bytes());
        m.extend_from_slice(&0u32.to_le_bytes());
        m.extend_from_slice(&sequence.to_le_bytes());
        m.extend_from_slice(&(body.len() as u32).to_le_bytes());
        m.extend_from_slice(body);
        m
    }

    fn words(w: &[u32]) -> Vec<u8> {
        w.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    /// An Airspy R2's server: 10 MSPS, its first two stages hidden (so 2.5 MSPS is the widest offered).
    fn r2() -> Info {
        Info { device: 1, serial: 0xAABB_CCDD, max_rate: 10_000_000, max_bandwidth: 10_000_000, stages: 8, max_gain: 21, min_hz: 24_000_000, max_hz: 1_800_000_000, resolution: 12, min_decimation: 2, forced_format: 0 }
    }

    fn info_bytes(i: &Info) -> Vec<u8> {
        words(&[i.device, i.serial, i.max_rate, i.max_bandwidth, i.stages, 3, i.max_gain, i.min_hz, i.max_hz, i.resolution, i.min_decimation, i.forced_format])
    }

    fn sync_bytes(can_control: u32, gain: u32, device_center: u32) -> Vec<u8> {
        words(&[can_control, gain, device_center, device_center, 0, 0, 0, 0, 0])
    }

    #[test]
    fn commands_are_laid_out_as_the_protocol_has_them() {
        let h = frame_hello("tr");
        assert_eq!(h, [0, 0, 0, 0, 6, 0, 0, 0, 0xA4, 0x06, 0, 2, b't', b'r'], "a hello: the command, its length, the version (2.0.1700), the name");
        assert_eq!(frame_setting(SETTING_IQ_FREQUENCY, 154_430_000), [2, 0, 0, 0, 8, 0, 0, 0, 101, 0, 0, 0, 0x30, 0x6A, 0x34, 0x09]);
        assert_eq!(frame_ping(), [3, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!((freq_wire(-5.0), freq_wire(154.43e6), freq_wire(1e12)), (0, 154_430_000, u32::MAX));
    }

    #[test]
    fn a_message_is_read_when_it_is_all_there() {
        let m = message(MSG_UINT8_IQ, 6, 7, &[1, 2, 3, 4]);
        assert_eq!(next_message(&m[..10]), Ok(None));
        assert_eq!(next_message(&m[..m.len() - 1]), Ok(None), "a body still to come");
        assert_eq!(next_message(&m), Ok(Some((Header { kind: MSG_UINT8_IQ, flags: 6, sequence: 7, body: 4 }, MSG_HEADER + 4))));
        let mut two = m.clone();
        two.extend_from_slice(&message(MSG_CLIENT_SYNC, 0, 8, &[]));
        let (_, first) = next_message(&two).unwrap().unwrap();
        assert_eq!(next_message(&two[first..]).unwrap().unwrap().0.kind, MSG_CLIENT_SYNC, "an empty body is a message");
    }

    #[test]
    fn a_stream_out_of_step_or_not_a_spyserver_is_refused() {
        let mut m = message(MSG_UINT8_IQ, 0, 0, &[]);
        m[3] = 9;
        assert!(next_message(&m).unwrap_err().contains("is that a SpyServer?"));
        let mut big = message(MSG_UINT8_IQ, 0, 0, &[]);
        big[16..20].copy_from_slice(&(MAX_BODY + 1).to_le_bytes());
        assert!(next_message(&big).unwrap_err().contains("out of step"));
        assert!(next_message(b"HTTP/1.1 400 Bad Request\r\n\r\n").is_err());
    }

    #[test]
    fn what_a_server_says_of_itself_is_read() {
        assert_eq!(parse_info(&info_bytes(&r2())), Ok(r2()));
        assert!(parse_info(&[0; 8]).is_err());
        assert_eq!(parse_sync(&sync_bytes(1, 14, 154_000_000)), Ok(ClientSync { can_control: true, gain: 14, device_center_hz: 154e6 }));
        assert!(parse_sync(&[0; 8]).is_err());
    }

    #[test]
    fn the_rates_are_the_top_one_halved_from_the_servers_floor() {
        let rates: Vec<f64> = r2().rates().iter().map(|r| r.1).collect();
        assert_eq!(rates, vec![2_500_000.0, 1_250_000.0, 625_000.0, 312_500.0, 156_250.0, 78_125.0, 39_063.0]);
        assert_eq!(r2().rates()[0].0, 2, "the first is the second stage");
        let rtl = Info { device: 3, max_rate: 2_400_000, stages: 6, min_decimation: 0, ..Default::default() };
        assert_eq!(rtl.rates()[..3], [(0, 2_400_000.0), (1, 1_200_000.0), (2, 600_000.0)]);
    }

    #[test]
    fn digital_gain_starts_where_the_reference_clients_do() {
        // An R2: 3.01 dB a stage, and a step below its top gain makes a dB more.
        assert!((r2().digital_gain_db(14, 2) - 13.02).abs() < 1e-9);
        assert!((r2().digital_gain_db(21, 2) - 6.02).abs() < 1e-9);
        let hf = Info { device: 2, max_gain: 8, ..Default::default() };
        assert!((hf.digital_gain_db(0, 3) - 9.03).abs() < 1e-9, "only an R2's gain steps count");
    }

    #[test]
    fn a_window_slides_inside_the_passband_not_the_sample_rate() {
        // An HF+ samples 768 kHz and hears 660: at 384 kHz that is 138 kHz either side, not 192.
        let hf = Info { device: 2, max_rate: 768_000, max_bandwidth: 660_000, ..Default::default() };
        assert_eq!(hf.slack_hz(384_000.0), 138_000.0);
        assert_eq!(r2().slack_hz(10e6), 0.0, "the whole receiver has no room to slide in");
        let silent = Info { max_rate: 2_400_000, max_bandwidth: 0, ..Default::default() };
        assert_eq!(silent.slack_hz(1_200_000.0), 600_000.0, "a server that states no bandwidth is taken at its rate");
    }

    #[test]
    fn samples_are_scaled_to_full_scale_with_the_servers_gain_taken_out() {
        // As a capture file's samples are read (8-bit offset around 127.5, as an RTL-SDR's).
        let mut out = Vec::new();
        decode(MSG_UINT8_IQ, &[255, 0], 1.0, &mut out).unwrap();
        assert_eq!(out, vec![Complex32::new(1.0, -1.0)]);
        // 20 dB of gain applied by the server is a factor of ten to divide out.
        decode(MSG_UINT8_IQ, &[255, 0], 10.0, &mut out).unwrap();
        assert_eq!(out, vec![Complex32::new(0.1, -0.1)]);
        let i16s: Vec<u8> = [16384i16, -32768].iter().flat_map(|v| v.to_le_bytes()).collect();
        decode(MSG_INT16_IQ, &i16s, 1.0, &mut out).unwrap();
        assert_eq!(out, vec![Complex32::new(0.5, -1.0)]);
        let floats: Vec<u8> = [0.25f32, -0.5].iter().flat_map(|v| v.to_le_bytes()).collect();
        decode(MSG_FLOAT_IQ, &floats, 2.0, &mut out).unwrap();
        assert_eq!(out, vec![Complex32::new(0.125, -0.25)]);
        // (A part of a sample at the end is not one.)
        decode(MSG_UINT8_IQ, &[128, 128, 7], 1.0, &mut out).unwrap();
        assert_eq!(out.len(), 1);
        assert!(decode(999, &[], 1.0, &mut out).is_err());
    }

    #[test]
    fn a_format_the_server_forces_is_taken_or_refused() {
        assert_eq!(forced_format(0), Ok(None));
        assert_eq!(forced_format(FORMAT_INT16), Ok(Some(SampleFormat::Cs16)));
        assert!(forced_format(FORMAT_INT24).unwrap_err().contains("24-bit"));
        assert!(forced_format(9).is_err());
    }

    /// A server that introduces itself as `info` (`can_control`, at `center`), and keeps the settings the client
    /// sends up to the one that turns the stream on. Gives the hello, then those settings (as (setting, value)).
    fn serve(info: Info, can_control: u32, center: u32) -> (u16, std::thread::JoinHandle<(Vec<u8>, Vec<(u32, u32)>)>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            // The hello: a command header, then its body.
            let mut head = [0u8; CMD_HEADER];
            s.read_exact(&mut head).unwrap();
            let mut hello = head.to_vec();
            let mut body = vec![0u8; u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize];
            s.read_exact(&mut body).unwrap();
            hello.extend_from_slice(&body);
            s.write_all(&message(MSG_DEVICE_INFO, 0, 0, &info_bytes(&info))).unwrap();
            s.write_all(&message(MSG_CLIENT_SYNC, 0, 1, &sync_bytes(can_control, 7, center))).unwrap();
            let mut settings = Vec::new();
            loop {
                let mut f = [0u8; CMD_HEADER + 8];
                if s.read_exact(&mut f).is_err() {
                    break;
                }
                let (st, v) = (u32::from_le_bytes([f[8], f[9], f[10], f[11]]), u32::from_le_bytes([f[12], f[13], f[14], f[15]]));
                settings.push((st, v));
                if st == SETTING_STREAMING_ENABLED {
                    break;
                }
            }
            (hello, settings)
        });
        (port, h)
    }

    fn cfg(port: u16, center_hz: f64, rate_hz: f64) -> SpyserverConfig {
        SpyserverConfig { server: format!("127.0.0.1:{port}"), center_hz, rate_hz, format: SampleFormat::Cs16, gain: 14, digital_gain_db: None }
    }

    #[test]
    fn it_connects_and_sets_the_stream_up_in_order() {
        let (port, server) = serve(r2(), 1, 100_000_000);
        let c = cfg(port, 154_430_000.0, 2.5e6);
        let (mut client, left) = connect(&c).unwrap();
        assert!(left.is_empty());
        assert_eq!((client.stage, client.rate, client.can_control), (2, 2.5e6, true));
        start(&mut client, &c).unwrap();
        let (hello, settings) = server.join().unwrap();
        assert_eq!(&hello[..4], &CMD_HELLO.to_le_bytes());
        assert_eq!(
            settings,
            vec![
                (SETTING_IQ_FORMAT, FORMAT_INT16),
                (SETTING_IQ_DECIMATION, 2),
                (SETTING_IQ_FREQUENCY, 154_430_000),
                (SETTING_STREAMING_MODE, STREAM_MODE_IQ_ONLY),
                (SETTING_GAIN, 14),
                (SETTING_IQ_DIGITAL_GAIN, 13),
                (SETTING_STREAMING_ENABLED, 1),
            ]
        );
    }

    #[test]
    fn asking_reads_the_rates_and_gain_steps_the_server_offers_and_sets_nothing() {
        let (port, server) = serve(r2(), 1, 100_000_000);
        let address = format!("127.0.0.1:{port}");
        let caps = ask(&address).unwrap();
        assert_eq!(caps.model, "Airspy R2 / Mini");
        assert_eq!(caps.rates, Rates::List(vec![2_500_000, 1_250_000, 625_000, 312_500, 156_250]), "whole Hz, the ones under 100 kHz left out");
        assert_eq!((caps.gains.steps.len(), caps.gains.steps[0], caps.gains.steps[21]), (22, 0.0, 21.0), "its gain steps, 0 to its highest");
        let (_, settings) = server.join().unwrap();
        assert!(settings.is_empty(), "{settings:?}");
        assert!(ask("127.0.0.1:1").is_err(), "nothing there");
        // Remembered, as any radio is when opened: a list of radios and the gain-tuning tool read it.
        assert_eq!(super::super::known(&key(&address)), Some(caps));
        assert_eq!(super::super::known(&key("never.asked:5555")), None);
    }

    #[test]
    fn a_gain_step_is_set_live_with_the_digital_gain_that_follows_it() {
        let (port, server) = serve(r2(), 1, 100_000_000);
        let (mut client, _) = connect(&cfg(port, 154e6, 2.5e6)).unwrap();
        let r = client.set_gain(Gain::Step(9)).unwrap();
        assert_eq!(r.gain, Some(Gain::Step(9)));
        assert_eq!(client.set_gain(Gain::Step(99)).unwrap().gain, Some(Gain::Step(21)), "to the server's highest");
        assert_eq!(client.set_gain(Gain::Db(20.0)).unwrap(), Regained::LEFT_ALONE, "a dB figure isn't a step");
        drop(client);
        let (_, settings) = server.join().unwrap();
        // An R2 at step 9, stage 2: (21 - 9) + 2 x 3.01 = 18 dB; at 21: 6.
        assert_eq!(settings, vec![(SETTING_GAIN, 9), (SETTING_IQ_DIGITAL_GAIN, 18), (SETTING_GAIN, 21), (SETTING_IQ_DIGITAL_GAIN, 6)]);
    }

    #[test]
    fn a_second_clients_gain_isnt_set() {
        let (port, server) = serve(r2(), 0, 154_000_000);
        let (mut client, _) = connect(&cfg(port, 154e6, 1.25e6)).unwrap();
        assert_eq!(client.set_gain(Gain::Step(9)).unwrap(), Regained::LEFT_ALONE);
        drop(client);
        assert!(server.join().unwrap().1.is_empty());
    }

    #[test]
    fn a_rate_the_server_doesnt_offer_is_refused_with_the_ones_it_does() {
        let (port, server) = serve(r2(), 1, 100_000_000);
        let e = connect(&cfg(port, 154e6, 3e6)).err().unwrap();
        assert!(e.contains("this SpyServer (Airspy R2 / Mini) runs at 2.5 / 1.25 / 0.625 / 0.313 / 0.156 MSPS, not 3"), "{e}");
        drop(server);
    }

    #[test]
    fn a_second_client_keeps_to_the_first_ones_window_and_its_gain() {
        // Another client has the receiver on 154 MHz: at 1.25 MSPS of its 10, a window may sit within ±4.375 MHz.
        let (port, server) = serve(r2(), 0, 154_000_000);
        let c = cfg(port, 156_000_000.0, 1.25e6);
        let (mut client, _) = connect(&c).unwrap();
        assert!(!client.can_control);
        assert_eq!(client.gain_index, 7, "the gain is the other client's");
        assert_eq!(client.reachable(170e6), 154e6 + 4_375_000.0);
        assert_eq!(client.reachable(100e6), 154e6 - 4_375_000.0);
        start(&mut client, &c).unwrap();
        let (_, settings) = server.join().unwrap();
        assert!(settings.iter().all(|s| s.0 != SETTING_GAIN), "a gain isn't sent to a receiver that isn't ours: {settings:?}");
        assert!(settings.contains(&(SETTING_IQ_FREQUENCY, 156_000_000)));
        // Out of reach, it says so at the start.
        let (port, server) = serve(r2(), 0, 154_000_000);
        let e = connect(&cfg(port, 460e6, 1.25e6)).err().unwrap();
        assert!(e.contains("another client has the receiver, on 154.000 MHz") && e.contains("±4375 kHz"), "{e}");
        drop(server);
    }

    #[test]
    fn a_server_that_isnt_one_is_said_so() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.write_all(b"HTTP/1.1 400 Bad Request\r\nServer: x\r\n\r\n").unwrap();
            std::thread::sleep(Duration::from_millis(200));
        });
        let e = connect(&cfg(port, 154e6, 2.5e6)).err().unwrap();
        assert!(e.contains("is that a SpyServer?"), "{e}");
        h.join().unwrap();
    }
}
