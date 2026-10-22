//! The wall clock: the time now, and how far to trust it (#61). What the
//! operating system says of its own clock (synchronized or not) and which
//! time service keeps it, what an SNTP query to a time server measures (the
//! offset itself), and what the radio systems' own clocks say — read, never
//! set: the OS's time service keeps the clock, this only watches it.
//!
//! Calls aren't stamped by the wall clock as it is now but by the recording's
//! clock: the wall clock at Start, carried on by a monotonic clock so that it
//! never jumps. The two part when the time service corrects the clock during
//! a recording ([`Health::drift_ms`]), and what call times are off by is the
//! recording clock's error: [`Health::problem`] judges that.
//!
//! [`Clock`] (`ctx.clock`) keeps the latest of it all and the one verdict
//! ([`Health::problem`]) the dashboard, its events and every finished call go by.

use std::net::{ToSocketAddrs, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The time now, Unix seconds.
pub fn unix_s() -> f64 {
    unix_s_of(SystemTime::now())
}

/// The time now, Unix milliseconds.
pub fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// A time (a file's), Unix seconds.
pub fn unix_s_of(t: SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// Off by this much or more, ms: worth a warning (`monitor.clockWarnMs`).
/// Calls from several recorders are matched by their times, and a quarter
/// second is already more than any of the time services leaves.
pub const WARN_MS: f64 = 250.0;
/// And this much or more a problem: 5 s, or 4 times the warning if that's more.
pub const BAD_MS: f64 = 5000.0;
/// A measurement older than this isn't believed any more (the time server
/// stopped answering): it's shown, but nothing is concluded from it.
const STALE_S: f64 = 3600.0;
/// A radio system's clock this far from ours is set to something else (its
/// local time, or nothing): it isn't gone by.
const RADIO_SANE_MS: f64 = 600_000.0;
/// A wall clock jump of this much or more between two looks is a step.
const STEP_MS: f64 = 500.0;
/// How often the time server is asked (a public pool asks for no more than
/// every minute or so), how soon again after it didn't answer, and how often
/// the OS's time service is asked what it follows.
const TIME_EVERY: Duration = Duration::from_secs(15 * 60);
const TIME_RETRY: Duration = Duration::from_secs(2 * 60);
const SERVICE_EVERY: Duration = Duration::from_secs(5 * 60);

/// A radio system's clock against the recording's, as the session hears it.
pub use trunk_app::session::RadioClock;

/// (warn, bad), ms, for a warning threshold `warn` (None: the default).
fn limits_for(warn: Option<f64>) -> (f64, f64) {
    let warn = warn.filter(|w| w.is_finite() && *w > 0.0).unwrap_or(WARN_MS);
    (warn, BAD_MS.max(4.0 * warn))
}

/// A radio system's clock near enough ours to be the right time at all (not
/// a local time, or unset).
fn sane(r: &RadioClock) -> bool {
    r.offset_ms.abs() < RADIO_SANE_MS
}

/// What the operating system says of its clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OsClock {
    /// Its time service says it's synchronized.
    pub synced: bool,
    /// How far off it could be at most, by its own account, ms (a loose bound).
    pub max_error_ms: f64,
}

/// An SNTP query's answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measured {
    /// The server's time less ours, ms: positive when this clock is behind.
    pub offset_ms: f64,
    /// The round trip, less the server's own time on it, ms.
    pub delay_ms: f64,
    /// The server's distance from a reference clock (1: it has one).
    pub stratum: u8,
}

/// The time service keeping the clock, as far as it says (Linux: chrony,
/// systemd-timesyncd).
#[derive(Clone, Debug, PartialEq)]
pub struct Service {
    pub name: &'static str,
    /// The server it follows (an address for chrony).
    pub server: Option<String>,
    pub stratum: Option<u8>,
    /// When it last heard from it, Unix s.
    pub last_sync: Option<f64>,
}

/// What's wrong with the clock: whose fault it is, for the interface to
/// point at the right line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Cause {
    /// The clock itself, measured against the time server.
    Clock,
    /// The clock calls are stamped by: right at Start, corrected since.
    Recording,
    /// Against the radio systems' clocks (no time server answer).
    Radio,
    /// The OS says it isn't synchronized.
    Os,
}

/// The verdict ([`Health::problem`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub bad: bool,
    pub cause: Cause,
    pub why: String,
}

/// What's known of the clock at one moment: the facts [`Health::problem`]
/// judges by, plain data (no clock read), so it's tested as it is.
#[derive(Clone, Debug, Default)]
pub struct Health {
    /// None: the OS doesn't say (Windows), or nothing read yet.
    pub os: Option<OsClock>,
    /// The time service, when one says.
    pub service: Option<Service>,
    /// The time server queried, and its last answer and when (Unix s).
    pub server: Option<String>,
    pub measured: Option<(Measured, f64)>,
    /// Why the last query failed, while it fails.
    pub error: Option<String>,
    /// While recording: the wall clock less the recording's, ms (positive:
    /// the wall clock was set ahead since Start, or the computer slept).
    pub drift_ms: Option<f64>,
    /// The radio systems' clocks, each site's latest.
    pub radio: Vec<RadioClock>,
    /// The warning threshold, ms (None: [`WARN_MS`]).
    pub warn_ms: Option<f64>,
}

/// ms in words: ms under a second, then seconds (as the dashboard's `ms()` writes it).
fn amount(ms: f64) -> String {
    let a = ms.abs();
    if a >= 10_000.0 {
        format!("{:.0} s", a / 1000.0)
    } else if a >= 1000.0 {
        format!("{:.1} s", a / 1000.0)
    } else {
        format!("{a:.0} ms")
    }
}

impl Health {
    /// The last measurement, while it's recent enough to go by.
    pub fn current(&self, now: f64) -> Option<Measured> {
        self.measured.filter(|(_, at)| now - at < STALE_S).map(|(m, _)| m)
    }

    /// The radio systems' word on the recording clock: the median of the
    /// recent, sane ones that vouch for their time, and a site that said it.
    pub fn radio_reference(&self, now: f64) -> Option<(String, f64)> {
        let mut v: Vec<&RadioClock> = self.radio.iter().filter(|r| r.valid && now - r.at_s < 600.0 && sane(r)).collect();
        v.sort_by(|a, b| a.offset_ms.total_cmp(&b.offset_ms));
        v.get(v.len() / 2).map(|r| (r.site.clone(), r.offset_ms))
    }

    /// (warn, bad), ms.
    pub fn limits(&self) -> (f64, f64) {
        limits_for(self.warn_ms)
    }

    /// What's wrong with the clock, if anything: the one place that
    /// decides. What's judged is the clock calls are stamped by; the time
    /// server is the reference, and without an answer from it, the radio
    /// systems that vouch for their time are.
    pub fn problem(&self, now: f64) -> Option<Problem> {
        let (warn, bad) = self.limits();
        let way = |ms: f64| if ms > 0.0 { "behind" } else { "ahead of" };
        let off = |ms: f64, cause, why: String| Some(Problem { bad: ms.abs() >= bad, cause, why });
        if let Some(m) = self.current(now) {
            let o = m.offset_ms;
            if o.abs() >= warn {
                return off(o, Cause::Clock, format!("The clock is {} {} {}: call times are off by as much", amount(o), way(o), self.server.as_deref().unwrap_or("the time server")));
            }
            // The clock is right now; the recording's may not be.
            if let Some(rec) = self.drift_ms.map(|d| o + d).filter(|r| r.abs() >= warn) {
                return off(rec, Cause::Recording, format!("Calls are being stamped {} {} the right time: the clock was corrected after recording started. Stop and Start again to stamp them by it", amount(rec), way(rec)));
            }
        } else if let Some((site, r)) = self.radio_reference(now).filter(|(_, r)| r.abs() >= warn) {
            return off(r, Cause::Radio, format!("Calls are being stamped {} {} {site}'s radio system clock: is this computer's clock synchronized?", amount(r), way(r)));
        }
        if self.os.is_some_and(|o| !o.synced) {
            return Some(Problem { bad: false, cause: Cause::Os, why: "The operating system says its clock isn't synchronized: is its time service (chrony, systemd-timesyncd, ntpd) running?".into() });
        }
        None
    }
}

/// The computer's clock, watched (`ctx.clock`): what's known of it, the
/// time server's answers, the recording's clock while recording, and the
/// verdict as of the last look ([`Clock::look`], once a monitor tick), which
/// the dashboard, the events and every finished call go by.
pub struct Clock {
    inner: Mutex<Inner>,
}

struct Inner {
    health: Health,
    /// The time server's answers.
    answers: crate::platform::Answers,
    /// The wall clock at Start (Unix ms) and the monotonic clock then, while recording.
    recording: Option<(f64, Instant)>,
    steps: StepWatch,
    problem: Option<Problem>,
}

impl Default for Clock {
    fn default() -> Self {
        let inner = Inner { health: Health::default(), answers: Default::default(), recording: None, steps: StepWatch::default(), problem: None };
        Clock { inner: Mutex::new(inner) }
    }
}

impl Clock {
    /// Ask `server` (SNTP, when there's one) and the OS's time service, each
    /// as often as it should be, on a thread of their own.
    pub fn start(self: &Arc<Self>, server: Option<String>) {
        let me = self.clone();
        let _ = std::thread::Builder::new().name("clock".into()).spawn(move || {
            let (mut next_time, mut next_service) = (Instant::now(), Instant::now());
            loop {
                if let Some(s) = server.as_deref().filter(|_| Instant::now() >= next_time) {
                    let ok = me.measure(s);
                    next_time = Instant::now() + if ok { TIME_EVERY } else { TIME_RETRY };
                }
                if Instant::now() >= next_service {
                    let service = service();
                    me.inner.lock().unwrap().health.service = service;
                    next_service = Instant::now() + SERVICE_EVERY;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    }

    /// Query `server` ([`sntp`]) and keep its answer, or why there's none.
    fn measure(&self, server: &str) -> bool {
        let (r, _) = sntp(server, Duration::from_secs(3));
        let now = unix_s();
        let mut x = self.inner.lock().unwrap();
        x.health.server = Some(server.to_string());
        x.answers.note(r.is_ok(), r.as_ref().ok().map(|m| m.delay_ms), now);
        let ok = r.is_ok();
        match r {
            Ok(m) => (x.health.measured, x.health.error) = (Some((m, now)), None),
            Err(e) => x.health.error = Some(e),
        }
        ok
    }

    /// A recording started with its clock at `epoch_ms` (Unix ms) at `at`; None: it stopped.
    pub fn recording(&self, clock: Option<(f64, Instant)>) {
        let mut x = self.inner.lock().unwrap();
        x.recording = clock;
        if clock.is_none() {
            x.health.radio.clear();
        }
    }

    /// The radio systems' clocks, as the recording last heard them.
    pub fn set_radio(&self, radio: Vec<RadioClock>) {
        self.inner.lock().unwrap().health.radio = radio;
    }

    /// Once a tick: read what the OS says, watch for a step, and judge with
    /// warning threshold `warn_ms`. The events it's worth (a step, the clock
    /// going off or right).
    pub fn look(&self, warn_ms: f64) -> Vec<trunk_app::stats::MonitorEvent> {
        use trunk_app::stats::MonitorEvent;
        let os = os_clock();
        let now = unix_s();
        let mut x = self.inner.lock().unwrap();
        let mut events = Vec::new();
        if let Some(by_ms) = x.steps.check() {
            events.push(MonitorEvent::ClockStep { by_ms: by_ms.round() });
        }
        x.health.os = os;
        x.health.warn_ms = Some(warn_ms);
        x.health.drift_ms = x.recording.map(|(epoch, at)| unix_ms() as f64 - (epoch + at.elapsed().as_secs_f64() * 1000.0));
        let problem = x.health.problem(now);
        let level = |p: &Option<Problem>| p.as_ref().map_or(0, |p| 1 + p.bad as u8);
        let (was, is) = (level(&x.problem), level(&problem));
        if is > was {
            events.extend(problem.as_ref().map(|p| MonitorEvent::ClockOff { why: p.why.clone(), bad: p.bad }));
        } else if is == 0 && was > 0 {
            events.push(MonitorEvent::ClockOk {});
        }
        x.problem = problem;
        events
    }

    /// Whether the clock calls are stamped by is off, as of the last look (a call's `clock_off`).
    pub fn off(&self) -> bool {
        self.inner.lock().unwrap().problem.is_some()
    }

    /// The clock into `sink` (`clock/…`) and the `platform` message's `clock`
    /// (what the OS can't say, into `missing`).
    pub fn report(&self, sink: &mut dyn trunk_core::metrics::Sink, missing: &mut impl FnMut(&str, &str)) -> serde_json::Value {
        let x = self.inner.lock().unwrap();
        let h = &x.health;
        let now = unix_s();
        match h.os {
            Some(o) => {
                sink.gauge("clock/synced", o.synced as u8 as f64);
                sink.gauge("clock/maxErrMs", o.max_error_ms);
            }
            None => missing("clockSynced", "this operating system doesn't say whether its clock is synchronized"),
        }
        let current = h.current(now);
        if let Some(m) = current {
            sink.gauge("clock/offsetMs", m.offset_ms);
            sink.gauge("clock/delayMs", m.delay_ms);
        }
        if let Some(d) = h.drift_ms {
            sink.gauge("clock/driftMs", d);
        }
        if let Some((_, r)) = h.radio_reference(now) {
            sink.gauge("clock/radioMs", r);
        }
        let round = |v: f64| (v * 10.0).round() / 10.0;
        serde_json::json!({
            "warnMs": h.limits().0,
            "os": h.os.map(|o| serde_json::json!({ "synced": o.synced, "maxErrorMs": round(o.max_error_ms) })),
            "service": h.service.as_ref().map(|s| serde_json::json!({ "name": s.name, "server": s.server, "stratum": s.stratum, "lastSync": s.last_sync })),
            "driftMs": h.drift_ms.map(round),
            "radio": h.radio.iter().map(|r| serde_json::json!({ "site": r.site, "offsetMs": round(r.offset_ms), "at": r.at_s.round(), "sane": sane(r), "valid": r.valid })).collect::<Vec<_>>(),
            "server": h.server,
            "measured": h.measured.map(|(m, at)| serde_json::json!({ "offsetMs": round(m.offset_ms), "delayMs": round(m.delay_ms), "stratum": m.stratum, "at": at, "current": current.is_some() })),
            "error": h.error,
            "problem": x.problem.as_ref().map(|p| serde_json::json!({ "level": if p.bad { "bad" } else { "warn" }, "cause": p.cause, "why": p.why })),
            "check": h.server.as_ref().map(|s| x.answers.row(s)),
        })
    }
}

/// Watches the wall clock against a monotonic one: a jump between two looks
/// is a step (the time service set it), or the computer slept.
struct StepWatch {
    last: (f64, Instant),
}

impl Default for StepWatch {
    fn default() -> Self {
        StepWatch { last: (unix_ms() as f64, Instant::now()) }
    }
}

impl StepWatch {
    /// The jump since the last look, ms (positive: forward), when it's a step.
    fn check(&mut self) -> Option<f64> {
        let now = (unix_ms() as f64, Instant::now());
        let jump = step(now.0 - self.last.0, now.1.duration_since(self.last.1).as_secs_f64() * 1000.0);
        self.last = now;
        jump
    }
}

/// The wall clock moved `wall_ms` while a monotonic one moved `mono_ms`: the
/// jump, when it's a step.
fn step(wall_ms: f64, mono_ms: f64) -> Option<f64> {
    let jump = wall_ms - mono_ms;
    (jump.abs() >= STEP_MS).then_some(jump)
}

/// The time service and what it follows: chrony (`chronyc -c -n tracking`),
/// else systemd-timesyncd (`timedatectl show-timesync`, and when it last
/// synchronized: the time of /run/systemd/timesync/synchronized). Linux
/// only; elsewhere, or when neither answers (a container), None.
pub fn service() -> Option<Service> {
    #[cfg(target_os = "linux")]
    {
        let run = |cmd: &str, args: &[&str]| std::process::Command::new(cmd).args(args).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        if let Some(s) = run("chronyc", &["-c", "-n", "tracking"]).and_then(|o| parse_chrony(&o)) {
            return Some(s);
        }
        let synced = std::fs::metadata("/run/systemd/timesync/synchronized").and_then(|m| m.modified()).ok().map(unix_s_of);
        run("timedatectl", &["show-timesync", "-p", "ServerName", "-p", "ServerAddress", "-p", "NTPMessage"]).and_then(|o| parse_timesyncd(&o, synced))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// `chronyc -c tracking`'s line: reference ID, name, stratum, reference
/// time (Unix s), …, leap status last.
fn parse_chrony(out: &str) -> Option<Service> {
    let f: Vec<&str> = out.lines().next()?.split(',').collect();
    if f.len() < 4 {
        return None;
    }
    let reftime = f[3].parse::<f64>().ok().filter(|t| *t > 0.0);
    Some(Service { name: "chrony", server: Some(f[1].to_string()).filter(|s| !s.is_empty()), stratum: f[2].parse().ok().filter(|s| *s > 0), last_sync: reftime })
}

/// `timedatectl show-timesync -p ServerName -p ServerAddress -p NTPMessage`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_timesyncd(out: &str, last_sync: Option<f64>) -> Option<Service> {
    let get = |k: &str| out.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')).map(str::trim).filter(|v| !v.is_empty());
    let (name, addr) = (get("ServerName"), get("ServerAddress"));
    if name.is_none() && addr.is_none() {
        return None;
    }
    let stratum = get("NTPMessage").and_then(|m| m.split(", ").find_map(|kv| kv.trim_start_matches("{ ").strip_prefix("Stratum="))).and_then(|v| v.parse().ok());
    let server = match (name, addr) {
        (Some(n), Some(a)) if n != a => format!("{n} ({a})"),
        (n, a) => n.or(a).unwrap_or_default().to_string(),
    };
    Some(Service { name: "systemd-timesyncd", server: Some(server), stratum, last_sync })
}

/// The kernel's clock state: Linux `adjtimex`, macOS `ntp_adjtime`, both
/// read-only with no modes set (no privileges). Every time service (chrony,
/// systemd-timesyncd, ntpd, macOS's timed) keeps it, so there's nothing to
/// know about which one runs. Inside a container it's the host's clock.
pub fn os_clock() -> Option<OsClock> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        // SAFETY: a zeroed timex with modes 0 only reads.
        let mut t: libc::timex = unsafe { std::mem::zeroed() };
        #[cfg(target_os = "linux")]
        let state = unsafe { libc::adjtimex(&mut t) };
        #[cfg(target_os = "macos")]
        let state = unsafe { libc::ntp_adjtime(&mut t) };
        (state >= 0).then(|| os_clock_from(state, t.status, t.maxerror as i64))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// The clock state from the call's answer: unsynchronized when it says
/// TIME_ERROR or STA_UNSYNC is set. `maxerror` is in microseconds.
fn os_clock_from(state: i32, status: i32, maxerror_us: i64) -> OsClock {
    const TIME_ERROR: i32 = 5;
    const STA_UNSYNC: i32 = 0x40;
    OsClock { synced: state != TIME_ERROR && status & STA_UNSYNC == 0, max_error_ms: maxerror_us as f64 / 1000.0 }
}

/// NTP counts from 1900; Unix from 1970.
const NTP_UNIX_S: f64 = 2_208_988_800.0;

/// One SNTP query (RFC 4330) to `server` (a name or address, port 123 unless
/// given): the offset and round trip. One 48-byte UDP packet each way; it
/// sets nothing. Also how long the name took to look up, ms.
pub fn sntp(server: &str, timeout: Duration) -> (Result<Measured, String>, Option<f64>) {
    let target = if server.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) && !server.ends_with(']') { server.to_string() } else { format!("{server}:123") };
    let looked = Instant::now();
    let addr = match target.to_socket_addrs().map(|mut a| a.next()) {
        Ok(Some(a)) => a,
        Ok(None) | Err(_) => return (Err(format!("can't find {server}")), None),
    };
    let dns = Some(looked.elapsed().as_secs_f64() * 1000.0);
    let r = (|| {
        let sock = UdpSocket::bind(if addr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }).map_err(|e| e.to_string())?;
        sock.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
        sock.connect(addr).map_err(|e| e.to_string())?;
        let t1 = unix_s();
        sock.send(&request(t1)).map_err(|e| e.to_string())?;
        let mut buf = [0u8; 68];
        let n = sock.recv(&mut buf).map_err(|e| if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) { "no answer".to_string() } else { e.to_string() })?;
        let t4 = unix_s();
        answer(&buf[..n], t1, t4)
    })()
    .map_err(|e| format!("{server}: {e}"));
    (r, dns)
}

/// A client request sent at `t1` (Unix s): version 4, mode 3, and `t1` as
/// its transmit time, which the server echoes as the originate time.
fn request(t1: f64) -> [u8; 48] {
    let mut p = [0u8; 48];
    p[0] = 4 << 3 | 3;
    p[40..48].copy_from_slice(&to_ntp(t1).to_be_bytes());
    p
}

/// The offset and delay from a server's answer to the request sent at `t1`
/// and received back at `t4` (Unix s).
fn answer(p: &[u8], t1: f64, t4: f64) -> Result<Measured, String> {
    if p.len() < 48 {
        return Err("a short answer".into());
    }
    let (leap, mode, stratum) = (p[0] >> 6, p[0] & 7, p[1]);
    if mode != 4 {
        return Err("not a server's answer".into());
    }
    if stratum == 0 {
        return Err(format!("the server declined ({})", String::from_utf8_lossy(&p[12..16]).trim_end_matches('\0')));
    }
    if leap == 3 || stratum > 15 {
        return Err("the server isn't synchronized itself".into());
    }
    let ts = |i: usize| u64::from_be_bytes(p[i..i + 8].try_into().unwrap());
    if ts(24) != to_ntp(t1) {
        return Err("an answer to another request".into());
    }
    let (t2, t3) = (from_ntp(ts(32)), from_ntp(ts(40)));
    Ok(Measured { offset_ms: ((t2 - t1) + (t3 - t4)) / 2.0 * 1000.0, delay_ms: ((t4 - t1) - (t3 - t2)).max(0.0) * 1000.0, stratum })
}

/// Unix seconds as an NTP timestamp (32.32 fixed point from 1900; era 1 from
/// 2036 wraps, as the protocol does).
fn to_ntp(unix: f64) -> u64 {
    let s = unix + NTP_UNIX_S;
    let secs = s.floor();
    ((secs as u64 & 0xffff_ffff) << 32) | ((s - secs) * 4_294_967_296.0) as u64
}

/// An NTP timestamp as Unix seconds: one with the top bit clear is era 1 (after 2036).
fn from_ntp(t: u64) -> f64 {
    let secs = (t >> 32) as f64 + if t >> 63 == 0 { 4_294_967_296.0 } else { 0.0 };
    secs + (t & 0xffff_ffff) as f64 / 4_294_967_296.0 - NTP_UNIX_S
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Health {
        /// The verdict as (bad, why).
        fn said(&self, now: f64) -> Option<(bool, String)> {
            self.problem(now).map(|p| (p.bad, p.why))
        }
    }

    /// A server's answer: received at `t2`, sent at `t3`, to a request sent at `t1`.
    fn reply(t1: f64, t2: f64, t3: f64, stratum: u8) -> [u8; 48] {
        let mut p = [0u8; 48];
        (p[0], p[1]) = (4 << 3 | 4, stratum);
        p[24..32].copy_from_slice(&to_ntp(t1).to_be_bytes());
        p[32..40].copy_from_slice(&to_ntp(t2).to_be_bytes());
        p[40..48].copy_from_slice(&to_ntp(t3).to_be_bytes());
        p
    }

    #[test]
    fn timestamps_round_trip_both_eras() {
        for t in [1_761_134_400.123_456, 2_085_978_495.5, 2_085_978_496.25, 2_200_000_000.75] {
            assert!((from_ntp(to_ntp(t)) - t).abs() < 1e-6, "{t}");
        }
    }

    #[test]
    fn offset_and_delay_from_the_four_times() {
        let t1 = 1_761_134_400.0;
        // The server is 0.5 s ahead; 20 ms each way; 1 ms in the server.
        let (t2, t3, t4) = (t1 + 0.020 + 0.5, t1 + 0.021 + 0.5, t1 + 0.041);
        let m = answer(&reply(t1, t2, t3, 2), t1, t4).unwrap();
        assert!((m.offset_ms - 500.0).abs() < 0.01, "{m:?}");
        assert!((m.delay_ms - 40.0).abs() < 0.01, "{m:?}");
        assert_eq!(m.stratum, 2);
        // The request carries its send time, which the answer must echo.
        assert_eq!(&request(t1)[40..48], &reply(t1, t2, t3, 2)[24..32]);
        assert_eq!(request(t1)[0], 0x23, "version 4, client");
    }

    #[test]
    fn answers_not_to_go_by() {
        let t1 = 1_761_134_400.0;
        let ok = reply(t1, t1, t1, 1);
        assert!(answer(&ok[..40], t1, t1).is_err(), "short");
        assert!(answer(&ok, t1 + 1.0, t1 + 1.0).unwrap_err().contains("another request"));
        let mut kod = ok;
        kod[1] = 0;
        kod[12..16].copy_from_slice(b"RATE");
        assert_eq!(answer(&kod, t1, t1).unwrap_err(), "the server declined (RATE)");
        let mut unsynced = ok;
        unsynced[0] |= 3 << 6;
        assert!(answer(&unsynced, t1, t1).unwrap_err().contains("isn't synchronized"));
        let mut client = ok;
        client[0] = 4 << 3 | 3;
        assert!(answer(&client, t1, t1).is_err());
    }

    #[test]
    fn the_kernels_word() {
        // As read on macOS (timed) and a Pi (systemd-timesyncd).
        assert_eq!(os_clock_from(0, 0x2081, 297_373), OsClock { synced: true, max_error_ms: 297.373 });
        assert_eq!(os_clock_from(0, 0x2001, 897_000), OsClock { synced: true, max_error_ms: 897.0 });
        assert!(!os_clock_from(5, 0x2041, 16_000_000).synced, "TIME_ERROR");
        assert!(!os_clock_from(0, 0x40, 0).synced, "STA_UNSYNC");
    }

    /// Asks a real time server (needs the network): `cargo test -- --ignored live`.
    #[test]
    #[ignore]
    fn live_query_and_os() {
        let (r, dns) = sntp("pool.ntp.org", Duration::from_secs(3));
        let m = r.unwrap();
        println!("pool.ntp.org: offset {:.1} ms, delay {:.1} ms, stratum {}, lookup {dns:?} ms; OS: {:?}", m.offset_ms, m.delay_ms, m.stratum, os_clock());
        assert!(m.offset_ms.abs() < 5000.0 && m.delay_ms < 3000.0 && (1..=15).contains(&m.stratum));
    }

    #[test]
    fn the_recording_clock_and_the_radio_systems() {
        let now = 1_761_134_400.0;
        let server = |offset_ms: f64, drift_ms: Option<f64>| Health {
            server: Some("pool.ntp.org".into()),
            measured: Some((Measured { offset_ms, delay_ms: 30.0, stratum: 2 }, now - 60.0)),
            drift_ms,
            ..Default::default()
        };
        // The clock was 2 s ahead at Start, then set right (back 2 s): it's right now, calls aren't.
        assert_eq!(server(10.0, Some(-2000.0)).said(now), Some((false, "Calls are being stamped 2.0 s ahead of the right time: the clock was corrected after recording started. Stop and Start again to stamp them by it".into())));
        assert_eq!(server(10.0, Some(-100.0)).said(now), None, "within the warning");
        assert_eq!(server(10.0, Some(-2000.0)).problem(now).map(|p| p.cause), Some(Cause::Recording), "the interface points at the recording's clock");
        assert_eq!(server(10.0, None).said(now), None, "not recording");
        // No time server answer: the radio systems' clocks are the reference.
        let radio = |offsets: &[f64], ago: f64| Health {
            radio: offsets.iter().enumerate().map(|(i, o)| RadioClock { site: format!("site{i}"), offset_ms: *o, at_s: now - ago, valid: true }).collect(),
            ..Default::default()
        };
        assert_eq!(radio(&[900.0, 850.0, 20.0], 30.0).said(now), Some((false, "Calls are being stamped 850 ms behind site1's radio system clock: is this computer's clock synchronized?".into())), "the median");
        assert_eq!(radio(&[900.0, 850.0, 20.0], 30.0).problem(now).map(|p| p.cause), Some(Cause::Radio));
        assert_eq!(radio(&[900.0], 3600.0).said(now), None, "heard too long ago");
        assert_eq!(radio(&[-4.0 * 3600e3], 30.0).said(now), None, "a system set to its local time isn't gone by");
        let mut unvouched = radio(&[900.0], 30.0);
        unvouched.radio[0].valid = false;
        assert_eq!(unvouched.said(now), None, "a time its system marks not valid is shown, not gone by");
        // With a time server answer, the radio systems are shown, not judged by.
        let both = Health { radio: radio(&[900.0], 30.0).radio, ..server(10.0, Some(0.0)) };
        assert_eq!(both.said(now), None);
    }

    #[test]
    fn a_step_is_the_wall_clock_moving_unlike_a_monotonic_one() {
        assert_eq!(step(1003.0, 1000.0), None, "a tick, slewed a little");
        assert_eq!(step(-1000.0, 1000.0), Some(-2000.0), "set back 2 s");
        assert_eq!(step(3_600_000.0 + 1000.0, 1000.0), Some(3_600_000.0), "an hour forward (or the computer slept)");
        let mut w = StepWatch::default();
        assert_eq!(w.check(), None, "nothing jumped just now");
    }

    #[test]
    fn the_threshold_is_the_users() {
        let now = 1_761_134_400.0;
        let at = |warn_ms: f64| Health { measured: Some((Measured { offset_ms: 120.0, delay_ms: 30.0, stratum: 2 }, now)), warn_ms: Some(warn_ms), ..Default::default() };
        assert_eq!(at(100.0).limits(), (100.0, 5000.0));
        assert!(at(100.0).said(now).is_some_and(|(bad, _)| !bad));
        assert_eq!(at(2000.0).limits(), (2000.0, 8000.0), "a problem at 4 times a big warning");
        assert_eq!(at(2000.0).said(now), None);
        assert_eq!(at(0.0).limits().0, WARN_MS, "nothing sensible: the default");
    }

    #[test]
    fn what_the_time_services_say() {
        // As chrony documents `chronyc -c tracking`.
        let c = parse_chrony("A29FC87B,162.159.200.123,3,1761134000.123456789,-0.000004520,0.000000948,0.000016137,-1.418,0.002,0.032,0.010551324,0.000548839,1024.2,Normal\n").unwrap();
        assert_eq!(c, Service { name: "chrony", server: Some("162.159.200.123".into()), stratum: Some(3), last_sync: Some(1_761_134_000.123456789) });
        assert_eq!(parse_chrony("506 Cannot talk to daemon"), None);
        // As read on the Pi testbed.
        let out = "ServerName=2.debian.pool.ntp.org\nServerAddress=45.33.53.84\nNTPMessage={ Leap=0, Version=4, Mode=4, Stratum=2, Precision=-25, RootDelay=610us, RootDispersion=488us, Reference=A0F510BC, Ignored=no, PacketCount=366, Jitter=5.553ms }\n";
        let t = parse_timesyncd(out, Some(1_792_679_444.0)).unwrap();
        assert_eq!(t, Service { name: "systemd-timesyncd", server: Some("2.debian.pool.ntp.org (45.33.53.84)".into()), stratum: Some(2), last_sync: Some(1_792_679_444.0) });
        assert_eq!(parse_timesyncd("ServerName=\nServerAddress=\nNTPMessage={}\n", None), None, "not synchronizing to anything");
    }

    /// One look judges once: the events say when it changes, and every
    /// finished call reads the same verdict.
    #[test]
    fn a_look_judges_once_for_everything() {
        use trunk_app::stats::MonitorEvent;
        let clock = Clock::default();
        let measure = |offset_ms: f64| clock.inner.lock().unwrap().health.measured = Some((Measured { offset_ms, delay_ms: 20.0, stratum: 2 }, unix_s()));
        let kinds = |v: Vec<MonitorEvent>| v.into_iter().filter(|e| !matches!(e, MonitorEvent::ClockStep { .. })).collect::<Vec<_>>();
        measure(12.0);
        assert!(kinds(clock.look(250.0)).is_empty());
        assert!(!clock.off());
        measure(-900.0);
        assert!(matches!(kinds(clock.look(250.0)).as_slice(), [MonitorEvent::ClockOff { bad: false, .. }]));
        assert!(clock.off(), "a finished call is marked");
        assert!(kinds(clock.look(250.0)).is_empty(), "said once");
        assert!(matches!(kinds(clock.look(1000.0)).as_slice(), [MonitorEvent::ClockOk {}]), "the user's threshold, at once");
        assert!(!clock.off());
        struct Gauges(Vec<String>);
        impl trunk_core::metrics::Sink for Gauges {
            fn counter(&mut self, _: &str, _: u64) {}
            fn gauge(&mut self, n: &str, _: f64) {
                self.0.push(n.into());
            }
        }
        let (mut sink, mut missing) = (Gauges(vec![]), |_: &str, _: &str| {});
        let v = clock.report(&mut sink, &mut missing);
        assert!(sink.0.contains(&"clock/offsetMs".to_string()));
        assert_eq!(v["warnMs"], 1000.0);
        assert!(v["problem"].is_null());
    }

    #[test]
    fn what_counts_as_a_problem() {
        let now = 1_761_134_400.0;
        let at = |offset_ms: f64, ago: f64| Health { server: Some("pool.ntp.org".into()), measured: Some((Measured { offset_ms, delay_ms: 30.0, stratum: 2 }, now - ago)), ..Default::default() };
        assert_eq!(at(12.0, 60.0).said(now), None);
        assert_eq!(at(-400.0, 60.0).said(now), Some((false, "The clock is 400 ms ahead of pool.ntp.org: call times are off by as much".into())));
        assert_eq!(at(65_000.0, 60.0).said(now), Some((true, "The clock is 65 s behind pool.ntp.org: call times are off by as much".into())));
        assert_eq!(at(-1840.0, 60.0).said(now).unwrap().1, "The clock is 1.8 s ahead of pool.ntp.org: call times are off by as much");
        assert_eq!(at(65_000.0, 2.0 * STALE_S).said(now), None, "an old measurement isn't gone by");
        let unsynced = Health { os: Some(OsClock { synced: false, max_error_ms: 16_000.0 }), ..Default::default() };
        assert!(unsynced.said(now).is_some_and(|(bad, why)| !bad && why.contains("isn't synchronized")));
        assert_eq!(Health::default().said(now), None, "nothing known: nothing said");
    }
}
