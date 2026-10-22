//! The wall clock: the time now, and how far to trust it (#61). What the
//! operating system says of its own clock (synchronized or not), and what an
//! SNTP query to a time server measures (the offset itself) — read, never
//! set: the OS's time service keeps the clock, this only watches it.
//!
//! [`health`] is the latest of both, for the dashboard, its events and
//! anything that stamps a time.

use std::net::{ToSocketAddrs, UdpSocket};
use std::sync::Mutex;
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

/// Measured off by more than this, ms: worth a warning. Calls from several
/// recorders are matched by their times, and a quarter second is already
/// more than any of the time services leaves.
pub const WARN_MS: f64 = 250.0;
/// And more than this: a problem.
pub const BAD_MS: f64 = 5000.0;
/// A measurement older than this isn't believed any more (the time server
/// stopped answering): it's shown, but nothing is concluded from it.
const STALE_S: f64 = 3600.0;

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

/// The latest of what's known of the clock.
#[derive(Clone, Debug, Default)]
pub struct Health {
    /// None: the OS doesn't say (Windows), or nothing read yet.
    pub os: Option<OsClock>,
    /// The time server queried, and its last answer and when (Unix s).
    pub server: Option<String>,
    pub measured: Option<(Measured, f64)>,
    /// Why the last query failed, while it fails.
    pub error: Option<String>,
}

impl Health {
    /// The last measurement, while it's recent enough to go by.
    pub fn current(&self, now: f64) -> Option<Measured> {
        self.measured.filter(|(_, at)| now - at < STALE_S).map(|(m, _)| m)
    }

    /// What's wrong with the clock, if anything: (bad, why). The one place
    /// that decides, for the dashboard's light and the events alike.
    pub fn problem(&self, now: f64) -> Option<(bool, String)> {
        if let Some(m) = self.current(now) {
            let off = m.offset_ms.abs();
            if off >= WARN_MS {
                // (As the dashboard writes it: ms under a second, then seconds.)
                let by = if off >= 10_000.0 { format!("{:.0} s", off / 1000.0) } else if off >= 1000.0 { format!("{:.1} s", off / 1000.0) } else { format!("{off:.0} ms") };
                let way = if m.offset_ms > 0.0 { "behind" } else { "ahead of" };
                return Some((off >= BAD_MS, format!("The clock is {by} {way} {}: call times are off by as much", self.server.as_deref().unwrap_or("the time server"))));
            }
        }
        if self.os.is_some_and(|o| !o.synced) {
            return Some((false, "The operating system says its clock isn't synchronized: is its time service (chrony, systemd-timesyncd, ntpd) running?".into()));
        }
        None
    }
}

static HEALTH: Mutex<Option<Health>> = Mutex::new(None);

/// The latest of what's known of the clock.
pub fn health() -> Health {
    HEALTH.lock().unwrap().clone().unwrap_or_default()
}

fn update(f: impl FnOnce(&mut Health)) {
    f(HEALTH.lock().unwrap().get_or_insert_with(Health::default));
}

/// Read what the OS says ([`os_clock`]) into [`health`].
pub fn read_os() -> Option<OsClock> {
    let os = os_clock();
    update(|h| h.os = os);
    os
}

/// Query `server` ([`sntp`]) and keep the answer, or why there's none, in
/// [`health`]. Returns the answer and how long the name took to look up.
pub fn measure(server: &str) -> (Result<Measured, String>, Option<f64>) {
    let (r, dns) = sntp(server, Duration::from_secs(3));
    let now = unix_s();
    update(|h| {
        h.server = Some(server.to_string());
        match &r {
            Ok(m) => (h.measured, h.error) = (Some((*m, now)), None),
            Err(e) => h.error = Some(e.clone()),
        }
    });
    (r, dns)
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
    fn what_counts_as_a_problem() {
        let now = 1_761_134_400.0;
        let at = |offset_ms: f64, ago: f64| Health { server: Some("pool.ntp.org".into()), measured: Some((Measured { offset_ms, delay_ms: 30.0, stratum: 2 }, now - ago)), ..Default::default() };
        assert_eq!(at(12.0, 60.0).problem(now), None);
        assert_eq!(at(-400.0, 60.0).problem(now), Some((false, "The clock is 400 ms ahead of pool.ntp.org: call times are off by as much".into())));
        assert_eq!(at(65_000.0, 60.0).problem(now), Some((true, "The clock is 65 s behind pool.ntp.org: call times are off by as much".into())));
        assert_eq!(at(-1840.0, 60.0).problem(now).unwrap().1, "The clock is 1.8 s ahead of pool.ntp.org: call times are off by as much");
        assert_eq!(at(65_000.0, 2.0 * STALE_S).problem(now), None, "an old measurement isn't gone by");
        let unsynced = Health { os: Some(OsClock { synced: false, max_error_ms: 16_000.0 }), ..Default::default() };
        assert!(unsynced.problem(now).is_some_and(|(bad, why)| !bad && why.contains("isn't synchronized")));
        assert_eq!(Health::default().problem(now), None, "nothing known: nothing said");
    }
}
