//! An RTL-SDR on another machine, through its `rtl_tcp` server: plain TCP, nothing to install.
//!
//! The protocol is rtl_tcp's own (osmocom's `rtl_tcp.c`): a 12-byte greeting (`RTL0`, the tuner's type and
//! its number of gain steps, big-endian), then the dongle's 8-bit offset I/Q, endlessly; the other way,
//! commands of five bytes (an opcode, a big-endian `u32`). It answers nothing, so what a source says it is
//! set to is what it asked for, and a command is done when the server gets to it.
//!
//! A gain is set to the nearest step the far end's tuner has: the server only says how many it has (in its
//! greeting), so the table is ours, by the tuner's type, and is used only where its length is the number the
//! server gave. A tuner not known here is sent the gain as asked, and takes what its driver makes of it.
//!
//! A server that answers with a block of `RSP0` first (`rsp_tcp -E`, an SDRplay) is read past it; its
//! 16-bit mode is refused, as the stream carries no word of it otherwise.
//!
//! Not checked as an RTL-SDR on USB is ([`crate::sdr::StreamCheck`]): it looks at blocks as the dongle's
//! transfers arrive, and a socket hands over whatever has arrived. A stream that stops is seen as one.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use trunk_app::config::{endpoint, RTL_TCP_PORT};

use super::Deadline;
use crate::sdr::{Applied, Control, Gain, Radio, Regained, Retuned, Sink, Tunable, Wire};

#[derive(Clone, Debug)]
pub struct RtlTcpConfig {
    /// The server: `host` or `host:port` (port 1234 if left off).
    pub address: String,
    pub center_hz: u64,
    pub rate_hz: u32,
    /// None = tuner AGC.
    pub gain_db: Option<f32>,
    pub ppm: f64,
    pub bias_tee: bool,
}

impl RtlTcpConfig {
    fn name(&self) -> String {
        format!("rtl_tcp {}", endpoint(&self.address, RTL_TCP_PORT))
    }
}

/// How long a connection may take to be made, and how long the server may take to say hello.
const CONNECT: Duration = Duration::from_secs(5);
/// How long to wait for the first bytes after the greeting, to see whether they're an `RSP0` block (a
/// server that has nothing to send yet has none).
const FIRST_BYTES: Duration = Duration::from_millis(500);
/// How long a read waits before the loop looks at `stop` and the controls again.
const POLL: Duration = Duration::from_millis(50);
/// A stream that has said nothing for this long has stopped.
const SILENT: Duration = Duration::from_secs(3);

const GREETING_LEN: usize = 12;
const RSP_LEN: usize = 45;

/// rtl_tcp's opcodes (the ones used here).
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
enum Cmd {
    Freq = 0x01,
    SampleRate = 0x02,
    /// 0: the tuner's own AGC, 1: manual.
    GainMode = 0x03,
    /// Tenths of a dB.
    Gain = 0x04,
    /// Whole ppm, signed.
    Ppm = 0x05,
    /// The demodulator's digital AGC.
    RtlAgc = 0x08,
    /// 0: off (the tuner), 1: the I branch, 2: the Q branch (HF).
    DirectSampling = 0x09,
    BiasTee = 0x0e,
}

fn frame(cmd: Cmd, arg: u32) -> [u8; 5] {
    let a = arg.to_be_bytes();
    [cmd as u8, a[0], a[1], a[2], a[3]]
}

/// What the greeting says of the far end's tuner.
#[derive(Clone, Debug, PartialEq)]
struct Greeting {
    tuner: &'static str,
    /// The tuner's type as the server numbers it ([`tuner_name`]).
    tuner_code: u32,
    /// How many gain steps it has.
    gains: u32,
}

fn tuner_name(code: u32) -> &'static str {
    match code {
        1 => "E4000",
        2 => "FC0012",
        3 => "FC0013",
        4 => "FC2580",
        5 => "R820T",
        6 => "R828D",
        _ => "an unknown tuner",
    }
}

fn parse_greeting(b: &[u8]) -> Result<Greeting, String> {
    if b.len() < GREETING_LEN {
        return Err("it hung up before it said hello".into());
    }
    if &b[..4] != b"RTL0" {
        let seen: String = b[..4].iter().map(|&c| if c.is_ascii_graphic() { c as char } else { '.' }).collect();
        return Err(format!("that isn't an rtl_tcp server (it said {seen:?}): check the address and port"));
    }
    let be = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    Ok(Greeting { tuner: tuner_name(be(4)), tuner_code: be(4), gains: be(8) })
}

/// The gain steps a tuner has, in tenths of a dB, as librtlsdr lists them: only for a tuner whose table is known
/// here and is as long as the server said (anything else is not this table, or not this tuner).
fn gain_steps(tuner_code: u32, count: u32) -> Option<&'static [i32]> {
    const E4000: &[i32] = &[-10, 15, 40, 65, 90, 115, 140, 165, 190, 215, 240, 290, 340, 420];
    const R82XX: &[i32] = &[0, 9, 14, 27, 37, 77, 87, 125, 144, 157, 166, 197, 207, 229, 254, 280, 297, 328, 338, 364, 372, 386, 402, 421, 434, 439, 445, 480, 496];
    let table = match tuner_code {
        1 => E4000,
        // (The R820T and the R828D share one.)
        5 | 6 => R82XX,
        _ => return None,
    };
    (table.len() as u32 == count).then_some(table)
}

/// The step nearest `db`, in tenths of a dB; the lower of two equally near (the less likely to overload).
fn nearest_step(steps: &[i32], db: f32) -> i32 {
    let want = (db * 10.0).round() as i32;
    steps.iter().copied().min_by_key(|&s| ((s - want).abs(), s)).unwrap_or(want)
}

/// An `RSP0` capabilities block says whether its samples are 16-bit (code 2 at byte 20).
fn rsp_is_16_bit(b: &[u8]) -> bool {
    b.len() >= RSP_LEN && u32::from_be_bytes([b[20], b[21], b[22], b[23]]) == 2
}

/// A connected server.
struct Conn {
    sock: TcpStream,
    /// Sample bytes read while looking for an `RSP0` block, ahead of the stream.
    first: Vec<u8>,
    greeting: Greeting,
    /// Its tuner's gain steps, where they're known ([`gain_steps`]).
    steps: Option<&'static [i32]>,
}

impl Conn {
    fn send(&mut self, cmd: Cmd, arg: u32) -> Result<(), String> {
        self.sock.write_all(&frame(cmd, arg)).map_err(|e| format!("send: {e}"))
    }

    /// Gain: the tuner's AGC, or manual at the step nearest `db` (as asked, where the steps aren't known).
    /// Gives what it was set to.
    fn gain(&mut self, g: Option<f32>) -> Result<Gain, String> {
        match g {
            None => {
                self.send(Cmd::GainMode, 0)?;
                Ok(Gain::Agc)
            }
            Some(db) => {
                let tenths = self.steps.map_or_else(|| (db * 10.0).round() as i32, |steps| nearest_step(steps, db));
                self.send(Cmd::GainMode, 1)?;
                self.send(Cmd::Gain, tenths as u32)?;
                Ok(Gain::Db(tenths as f32 / 10.0))
            }
        }
    }
}

/// Reads until `buf` is full, the server hangs up or `within` passes: how many bytes came, and whether
/// it was the time that ran out (what came is kept either way: a byte lost here would swap I and Q).
fn read_up_to(sock: &mut TcpStream, buf: &mut [u8], within: Duration) -> std::io::Result<(usize, bool)> {
    let until = Instant::now() + within;
    let mut n = 0;
    while n < buf.len() {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok((n, true));
        }
        sock.set_read_timeout(Some(left))?;
        match sock.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            // (A read that times out is WouldBlock on Unix, TimedOut on Windows.)
            Err(e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e),
        }
    }
    Ok((n, false))
}

/// The gain steps, dB, of the tuner each server (by its address as entered) reported when last connected to: what
/// the gain-tuning tool walks, since a source's config doesn't say which tuner is behind it.
static TUNERS: Mutex<Option<HashMap<String, Vec<f64>>>> = Mutex::new(None);

fn remember(address: &str, steps: Option<&[i32]>) {
    let mut t = TUNERS.lock().unwrap_or_else(|e| e.into_inner());
    let map = t.get_or_insert_with(HashMap::new);
    match steps {
        Some(s) => map.insert(address.trim().to_string(), s.iter().map(|&x| f64::from(x) / 10.0).collect()),
        // (A server that now reports a tuner this has no table for: what it said before is no longer so.)
        None => map.remove(address.trim()),
    };
}

/// The gain steps, dB, of the tuner the server at `address` reported when last connected to; none when it hasn't
/// been, or reported a tuner this has no table for.
pub fn steps(address: &str) -> Option<Vec<f64>> {
    TUNERS.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(address.trim()).cloned()
}

fn connect(cfg: &RtlTcpConfig) -> Result<Conn, String> {
    let name = cfg.name();
    let mut sock = super::dial(&name, &endpoint(&cfg.address, RTL_TCP_PORT), CONNECT)?;
    let mut hello = [0u8; GREETING_LEN];
    let (n, late) = read_up_to(&mut sock, &mut hello, CONNECT).map_err(|e| format!("{name}: {e}"))?;
    if late && n < GREETING_LEN {
        return Err(format!("{name}: it connected but said nothing: is that an rtl_tcp server? (One that has a client already doesn't answer another.)"));
    }
    let greeting = parse_greeting(&hello[..n]).map_err(|e| format!("{name}: {e}"))?;
    // Whatever comes first: an RSP0 block (read past), or the dongle's samples (kept).
    let mut first = vec![0u8; 4];
    let (got, _) = read_up_to(&mut sock, &mut first, FIRST_BYTES).map_err(|e| format!("{name}: {e}"))?;
    first.truncate(got);
    if first.as_slice() == b"RSP0" {
        let mut rest = [0u8; RSP_LEN - 4];
        let (n, _) = read_up_to(&mut sock, &mut rest, CONNECT).map_err(|e| format!("{name}: {e}"))?;
        if n < rest.len() {
            return Err(format!("{name}: its capabilities block was cut short"));
        }
        let mut block = b"RSP0".to_vec();
        block.extend_from_slice(&rest[..n]);
        if rsp_is_16_bit(&block) {
            return Err(format!("{name} is sending 16-bit samples (rsp_tcp -b 16), which are not read here: start it without -b 16"));
        }
        first.clear();
    }
    let steps = gain_steps(greeting.tuner_code, greeting.gains);
    remember(&cfg.address, steps);
    Ok(Conn { sock, first, greeting, steps })
}

/// Everything a connection is told before it streams, and the gain it ended up at. A server keeps what its last
/// client left (direct sampling, the digital AGC and the bias tee are not reset between clients), so each is stated.
fn configure(c: &mut Conn, cfg: &RtlTcpConfig) -> Result<Gain, String> {
    c.send(Cmd::DirectSampling, 0)?;
    c.send(Cmd::RtlAgc, 0)?;
    c.send(Cmd::Ppm, cfg.ppm.round() as i32 as u32)?;
    c.send(Cmd::SampleRate, cfg.rate_hz)?;
    c.send(Cmd::Freq, cfg.center_hz as u32)?;
    let set = c.gain(cfg.gain_db)?;
    c.send(Cmd::BiasTee, u32::from(cfg.bias_tee))?;
    Ok(set)
}

/// The most a tuner takes (rtl_tcp's frequency is a `u32`).
fn tunable_hz(hz: f64) -> Result<u32, String> {
    if hz.is_finite() && (0.0..=u32::MAX as f64).contains(&hz) {
        Ok(hz.round() as u32)
    } else {
        Err(format!("an RTL-SDR can't tune to {:.4} MHz", hz / 1e6))
    }
}

impl Tunable for Conn {
    fn tune(&mut self, hz: f64) -> Result<Retuned, String> {
        let want = tunable_hz(hz)?;
        self.send(Cmd::Freq, want)?;
        // (It keeps sampling through a retune, and answers nothing: the centre is as asked.)
        Ok(Retuned { hz: want as f64, gap: 0 })
    }
    fn set_gain(&mut self, g: Gain) -> Result<Regained, String> {
        let db = match g {
            Gain::Agc => None,
            Gain::Db(db) => Some(db),
            Gain::Stages(_) | Gain::Preset(..) | Gain::Step(_) => return Ok(Regained::LEFT_ALONE),
        };
        let set = self.gain(db)?;
        Ok(Regained { gap: 0, gain: Some(set) })
    }
}

impl Radio for RtlTcpConfig {
    const BACKOFF: Duration = Duration::from_secs(3);
    fn rate_hz(&self) -> f64 {
        self.rate_hz as f64
    }
    fn retune_to(&mut self, hz: f64) {
        self.center_hz = hz as u64;
    }
    fn gain_to(&mut self, g: Gain) {
        match g {
            Gain::Agc => self.gain_db = None,
            Gain::Db(db) => self.gain_db = Some(db),
            Gain::Stages(_) | Gain::Preset(..) | Gain::Step(_) => {}
        }
    }
    fn gain(&self) -> Option<Gain> {
        Some(self.gain_db.map_or(Gain::Agc, Gain::Db))
    }
    fn stream_once(&self, sink: &mut Sink, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
        stream_once(self, sink, stop, ctl)
    }
}

fn stream_once(cfg: &RtlTcpConfig, sink: &mut Sink, stop: &AtomicBool, ctl: Option<&Control>) -> Result<(), String> {
    let name = cfg.name();
    // (Not under the lock radios open under: it keeps USB radios from being opened over each other.)
    let mut conn = {
        let cfg = cfg.clone();
        super::open(&name, Deadline::Loads, false, move || connect(&cfg))?
    };
    log::info!("{name}: connected, a {} tuner with {} gain steps", conn.greeting.tuner, conn.greeting.gains);
    let set = configure(&mut conn, cfg)?;
    match (cfg.gain_db, set) {
        (Some(asked), Gain::Db(step)) if conn.steps.is_some() && (asked - step).abs() > 0.05 => log::info!("{name}: gain {asked} dB is set to the nearest step the tuner has, {step} dB"),
        (Some(_), _) if conn.steps.is_none() => log::info!("{name}: the gain steps of a {} tuner with {} aren't known here: the gain is sent as asked", conn.greeting.tuner, conn.greeting.gains),
        _ => {}
    }
    sink.gained(cfg.gain(), Some(set));
    conn.sock.set_read_timeout(Some(POLL)).map_err(|e| format!("{name}: {e}"))?;

    let mut buf = vec![0u8; 64 << 10];
    // A byte left over when a read ends between an I and its Q: the pair is whole at the next read. (Sent
    // apart, they would swap I and Q for the rest of the stream.)
    let mut carry: Option<u8> = None;
    let mut heard = Instant::now();
    let mut ahead = std::mem::take(&mut conn.first);
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        if let Some(c) = ctl {
            match c.apply(&mut conn, cfg.center_hz as f64, sink) {
                Err(e) => break Err(format!("{name}: {e}")),
                Ok(Applied::Closed) => break Ok(()),
                Ok(_) => {}
            }
        }
        let start = usize::from(carry.is_some());
        if let Some(b) = carry {
            // (Kept in `carry` until a read has gone through: one that times out is made again.)
            buf[0] = b;
        }
        let got = if !ahead.is_empty() {
            let n = ahead.len().min(buf.len() - start);
            buf[start..start + n].copy_from_slice(&ahead[..n]);
            ahead.drain(..n);
            n
        } else {
            match conn.sock.read(&mut buf[start..]) {
                Ok(0) => break Err(format!("{name}: the server closed the connection")),
                Ok(n) => n,
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {
                    if heard.elapsed() > SILENT {
                        break Err(format!("{name}: no samples for {} s", SILENT.as_secs()));
                    }
                    continue;
                }
                Err(e) => break Err(format!("{name}: {e}")),
            }
        };
        let now = Instant::now();
        heard = now;
        let mut end = start + got;
        carry = None;
        if end % 2 == 1 {
            end -= 1;
            carry = Some(buf[end]);
        }
        if end > 0 && !sink.bytes(Wire::Offset, &buf[..end], 0, now) {
            break Ok(());
        }
    };
    if cfg.bias_tee {
        // (Best effort: the bias tee is left on for whoever connects next otherwise.)
        let _ = conn.send(Cmd::BiasTee, 0);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn commands_are_an_opcode_then_a_big_endian_argument() {
        assert_eq!(frame(Cmd::Freq, 154_430_000), [0x01, 0x09, 0x34, 0x6A, 0x30]);
        assert_eq!(frame(Cmd::SampleRate, 2_400_000), [0x02, 0x00, 0x24, 0x9F, 0x00]);
        assert_eq!(frame(Cmd::GainMode, 1), [0x03, 0, 0, 0, 1]);
        assert_eq!(frame(Cmd::BiasTee, 0), [0x0e, 0, 0, 0, 0]);
        // A negative ppm is its two's complement.
        assert_eq!(frame(Cmd::Ppm, (-3i32) as u32), [0x05, 0xFF, 0xFF, 0xFF, 0xFD]);
        assert_eq!(frame(Cmd::Gain, (19.7f32 * 10.0).round() as i32 as u32), [0x04, 0, 0, 0, 197]);
    }

    #[test]
    fn the_greeting_names_the_tuner_and_refuses_what_isnt_one() {
        let mut g = b"RTL0".to_vec();
        g.extend_from_slice(&5u32.to_be_bytes());
        g.extend_from_slice(&29u32.to_be_bytes());
        assert_eq!(parse_greeting(&g), Ok(Greeting { tuner: "R820T", tuner_code: 5, gains: 29 }));
        assert!(parse_greeting(b"RTL0").unwrap_err().contains("hung up"));
        let e = parse_greeting(b"HTTP/1.1 400 Bad").unwrap_err();
        assert!(e.contains("isn't an rtl_tcp server") && e.contains("HTTP"), "{e}");
        let mut unknown = b"RTL0".to_vec();
        unknown.extend_from_slice(&99u32.to_be_bytes());
        unknown.extend_from_slice(&0u32.to_be_bytes());
        assert_eq!(parse_greeting(&unknown).unwrap().tuner, "an unknown tuner");
    }

    #[test]
    fn a_gain_goes_to_the_nearest_step_the_tuner_has() {
        let r820t = gain_steps(5, 29).unwrap();
        assert_eq!(r820t.len(), 29);
        assert_eq!((nearest_step(r820t, 25.4), nearest_step(r820t, 20.0), nearest_step(r820t, 49.6)), (254, 197, 496), "an exact step, between steps, the top");
        assert_eq!((nearest_step(r820t, 60.0), nearest_step(r820t, -5.0), nearest_step(r820t, 0.4)), (496, 0, 0), "past either end");
        assert_eq!(nearest_step(r820t, 8.2), 77, "equally near 7.7 and 8.7: the lower");
        assert_eq!(nearest_step(gain_steps(1, 14).unwrap(), 0.0), -10, "an E4000's steps run below 0");
        // A table is used only where the server's count says it is the one: another count, another tuner.
        assert!(gain_steps(5, 28).is_none() && gain_steps(6, 29).is_some() && gain_steps(3, 23).is_none() && gain_steps(99, 29).is_none());
    }

    #[test]
    fn an_rsp_block_says_whether_it_is_sixteen_bit() {
        let mut b = vec![0u8; RSP_LEN];
        b[..4].copy_from_slice(b"RSP0");
        assert!(!rsp_is_16_bit(&b));
        b[23] = 2;
        assert!(rsp_is_16_bit(&b));
        assert!(!rsp_is_16_bit(&b[..20]));
    }

    #[test]
    fn a_frequency_is_one_a_tuner_can_be_asked_for() {
        assert_eq!(tunable_hz(154.43e6), Ok(154_430_000));
        assert!(tunable_hz(-1.0).is_err() && tunable_hz(5e9).is_err() && tunable_hz(f64::NAN).is_err());
    }

    /// A server that says hello, optionally sends an RSP0 block, then streams `samples`, and keeps what the
    /// client sends it.
    fn serve(rsp: bool, samples: Vec<u8>) -> (u16, std::thread::JoinHandle<Vec<u8>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut hello = b"RTL0".to_vec();
            hello.extend_from_slice(&5u32.to_be_bytes());
            hello.extend_from_slice(&29u32.to_be_bytes());
            s.write_all(&hello).unwrap();
            if rsp {
                let mut b = vec![0u8; RSP_LEN];
                b[..4].copy_from_slice(b"RSP0");
                s.write_all(&b).unwrap();
            }
            s.write_all(&samples).unwrap();
            // The client's commands, until it hangs up (the timeout only stops a hung test; a client that waits for samples that never come must not find the server gone).
            let mut got = Vec::new();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let _ = s.read_to_end(&mut got);
            got
        });
        (port, h)
    }

    fn cfg(port: u16) -> RtlTcpConfig {
        RtlTcpConfig { address: format!("127.0.0.1:{port}"), center_hz: 154_430_000, rate_hz: 2_400_000, gain_db: Some(19.7), ppm: -2.4, bias_tee: false }
    }

    #[test]
    fn it_connects_and_says_what_it_wants_set() {
        let samples: Vec<u8> = (0..200u8).collect();
        let (port, server) = serve(false, samples.clone());
        let c = cfg(port);
        let mut conn = connect(&c).unwrap();
        assert_eq!(conn.greeting.tuner, "R820T");
        assert_eq!(steps(&c.address).map(|s| s.len()), Some(29), "what the server reported is kept for the gain-tuning tool");
        assert_eq!(steps(&c.address).unwrap()[11], 19.7);
        assert_eq!(conn.first, samples[..4], "the first bytes are samples, and are kept");
        configure(&mut conn, &c).unwrap();
        drop(conn);
        let got = server.join().unwrap();
        let cmds: Vec<[u8; 5]> = got.chunks(5).map(|c| c.try_into().unwrap()).collect();
        assert_eq!(
            cmds,
            vec![
                frame(Cmd::DirectSampling, 0),
                frame(Cmd::RtlAgc, 0),
                frame(Cmd::Ppm, (-2i32) as u32),
                frame(Cmd::SampleRate, 2_400_000),
                frame(Cmd::Freq, 154_430_000),
                frame(Cmd::GainMode, 1),
                frame(Cmd::Gain, 197),
                frame(Cmd::BiasTee, 0),
            ]
        );
    }

    #[test]
    fn it_sets_the_nearest_step_and_says_which() {
        let (port, server) = serve(false, Vec::new());
        let mut c = cfg(port);
        c.gain_db = Some(20.0);
        let mut conn = connect(&c).unwrap();
        let Gain::Db(set) = configure(&mut conn, &c).unwrap() else { panic!("not a fixed gain") };
        assert_eq!(set, 19.7);
        // And live, as a retune's gain is.
        let Gain::Db(live) = conn.gain(Some(30.0)).unwrap() else { panic!("not a fixed gain") };
        assert_eq!(live, 29.7);
        assert!(matches!(conn.gain(None), Ok(Gain::Agc)));
        drop(conn);
        let got = server.join().unwrap();
        let cmds: Vec<[u8; 5]> = got.chunks(5).map(|c| c.try_into().unwrap()).collect();
        assert!(cmds.contains(&frame(Cmd::Gain, 197)) && cmds.contains(&frame(Cmd::Gain, 297)), "the steps were sent, not the gains asked: {cmds:?}");
        assert!(!cmds.contains(&frame(Cmd::Gain, 200)));
    }

    #[test]
    fn an_rsp_block_is_read_past() {
        let samples: Vec<u8> = (0..64u8).collect();
        let (port, server) = serve(true, samples.clone());
        let conn = connect(&cfg(port)).unwrap();
        assert!(conn.first.is_empty(), "the block isn't samples");
        let mut sock = conn.sock;
        let mut next = [0u8; 8];
        sock.read_exact(&mut next).unwrap();
        assert_eq!(next, samples[..8], "the stream carries on from the block's end");
        drop(sock);
        server.join().unwrap();
    }

    #[test]
    fn a_server_that_isnt_one_is_said_so() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").unwrap();
        });
        let e = connect(&cfg(port)).err().unwrap();
        assert!(e.contains("isn't an rtl_tcp server"), "{e}");
        h.join().unwrap();
    }

    #[test]
    fn nothing_listening_is_said_so() {
        // (A port that was just free.)
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let e = connect(&cfg(port)).err().unwrap();
        assert!(e.contains("can't connect"), "{e}");
    }
}
