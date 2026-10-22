//! The computer the recorder runs on: CPU (the machine's and ours), memory
//! and its pressure, the recordings' disk, temperature, the network's
//! traffic, whether the internet answers and how far off the clock is
//! ([`Probes`], [`crate::clock`]). Container-aware:
//! inside Docker the CPU and memory limits are the cgroup's, and the disks
//! are the mounted volumes. Whatever this platform can't tell is left out
//! and listed in `unavailable` with why, so the dashboard greys it out.
//!
//! Reports into the same [`Sink`] as the engine (series `plat/…`, `net/…`,
//! `clock/…`).

use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sysinfo::{Components, Disks, Networks, Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use trunk_core::metrics::{key_part, Sink};

/// How often the connectivity probes run.
pub const PROBE_EVERY: Duration = Duration::from_secs(15);
/// How often the time server is asked (a public pool asks for no more than
/// every minute or so), and how soon again after it didn't answer.
const TIME_EVERY: Duration = Duration::from_secs(15 * 60);
const TIME_RETRY: Duration = Duration::from_secs(2 * 60);
/// How often the OS's time service is asked what it follows.
const SERVICE_EVERY: Duration = Duration::from_secs(5 * 60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// Disks and temperatures change slowly: this often.
const SLOW_EVERY: Duration = Duration::from_secs(30);

/// What kind of container we're in, if any.
pub fn container() -> Option<&'static str> {
    if Path::new("/run/.containerenv").exists() {
        return Some("podman");
    }
    if Path::new("/.dockerenv").exists() {
        return Some("docker");
    }
    let cg = std::fs::read_to_string("/proc/1/cgroup").unwrap_or_default();
    container_from_cgroup(&cg)
}

fn container_from_cgroup(cg: &str) -> Option<&'static str> {
    if cg.contains("kubepods") {
        Some("kubernetes")
    } else if cg.contains("docker") || cg.contains("containerd") {
        Some("docker")
    } else if cg.contains("lxc") {
        Some("lxc")
    } else {
        None
    }
}

/// cgroup v2 `cpu.max` ("max 100000" or "150000 100000") as cores; v1's quota and period.
fn cpu_limit_cores() -> Option<f64> {
    if let Ok(s) = std::fs::read_to_string("/sys/fs/cgroup/cpu.max") {
        return parse_cpu_max(&s);
    }
    let q: f64 = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_quota_us").ok()?.trim().parse().ok()?;
    let p: f64 = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_period_us").ok()?.trim().parse().ok()?;
    (q > 0.0 && p > 0.0).then(|| q / p)
}

fn parse_cpu_max(s: &str) -> Option<f64> {
    let mut it = s.split_whitespace();
    let (q, p) = (it.next()?, it.next()?);
    let (q, p): (f64, f64) = (q.parse().ok()?, p.parse().ok()?);
    (p > 0.0).then(|| q / p)
}

/// Time the cgroup was held back for its CPU limit, µs (v2 `cpu.stat`).
fn throttled_us() -> Option<u64> {
    let s = std::fs::read_to_string("/sys/fs/cgroup/cpu.stat").ok()?;
    s.lines().find_map(|l| l.strip_prefix("throttled_usec ")).and_then(|v| v.trim().parse().ok())
}

/// Linux PSI: share of the last 10 s some task waited for memory, %.
fn memory_psi() -> Option<f64> {
    let s = std::fs::read_to_string("/sys/fs/cgroup/memory.pressure").or_else(|_| std::fs::read_to_string("/proc/pressure/memory")).ok()?;
    parse_psi(&s)
}

fn parse_psi(s: &str) -> Option<f64> {
    let line = s.lines().find(|l| l.starts_with("some"))?;
    line.split_whitespace().find_map(|f| f.strip_prefix("avg10=")).and_then(|v| v.parse().ok())
}

/// macOS: the kernel's memory pressure level (1 normal, 2 warning, 4 critical) as 0, 1, 2.
#[cfg(target_os = "macos")]
fn memory_level() -> Option<f64> {
    let mut v: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    let name = c"kern.memorystatus_vm_pressure_level";
    let r = unsafe { libc::sysctlbyname(name.as_ptr(), &mut v as *mut _ as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) };
    (r == 0).then_some(match v {
        4 => 2.0,
        2 => 1.0,
        _ => 0.0,
    })
}
#[cfg(not(target_os = "macos"))]
fn memory_level() -> Option<f64> {
    None
}

/// Raspberry Pi firmware: under-voltage / throttling now (bits 0–3 of get_throttled).
fn pi_throttled() -> Option<u32> {
    let s = std::fs::read_to_string("/sys/devices/platform/soc/soc:firmware/get_throttled").ok()?;
    u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok().map(|v| v & 0xF)
}

/// What one of the [`Probes`] does each time.
#[derive(Clone, Debug, PartialEq)]
pub enum Check {
    /// A TCP connect to `host:port`: does the internet answer (`net/…`).
    Connect(String),
    /// An SNTP query to a time server: how far off the clock is (`clock/…`,
    /// [`crate::clock::measure`]). A time server that doesn't answer isn't
    /// the internet down (UDP port 123 is often blocked): it says nothing of `net/up`.
    Time(String),
    /// Ask the OS's time service what it follows ([`crate::clock::read_service`]).
    Service,
}

impl Check {
    fn target(&self) -> &str {
        match self {
            Check::Connect(t) | Check::Time(t) => t,
            Check::Service => "time service",
        }
    }

    /// How long until it runs again, after it answered or not.
    fn every(&self, ok: bool) -> Duration {
        match self {
            Check::Connect(_) => PROBE_EVERY,
            Check::Time(_) => if ok { TIME_EVERY } else { TIME_RETRY },
            Check::Service => SERVICE_EVERY,
        }
    }

    /// (answered, round trip ms, name lookup ms).
    fn run(&self) -> (bool, Option<f64>, Option<f64>) {
        match self {
            Check::Connect(t) => probe(t),
            Check::Time(server) => {
                let (r, dns) = crate::clock::measure(server);
                (r.is_ok(), r.ok().map(|m| m.delay_ms), dns)
            }
            Check::Service => (crate::clock::read_service().is_some(), None, None),
        }
    }
}

/// One check and how it's been answering.
#[derive(Clone, Debug)]
struct Probe {
    check: Check,
    ok: Option<bool>,
    rtt_ms: Option<f64>,
    last_ok: Option<f64>,
    last_fail: Option<f64>,
    /// When it went down (Unix s), while down.
    down_since: Option<f64>,
    tries: u64,
    fails: u64,
}

/// The checks ([`Check`]), each on its own schedule on one thread: a TCP
/// connect to each internet target (no ICMP: that needs privileges), timed,
/// and how long a name took to resolve; and the time server's query.
#[derive(Default)]
pub struct Probes {
    probes: Mutex<Vec<Probe>>,
    dns_ms: Mutex<Option<f64>>,
}

pub type Transition = (String, bool, f64);

impl Probes {
    /// Run `checks` on a thread of their own, each as often as it says
    /// ([`Check::every`]); `changed(target, up, down_for_s)` hears each up/down
    /// change of an internet check.
    pub fn start(checks: Vec<Check>, changed: impl Fn(Transition) + Send + 'static) -> Arc<Probes> {
        let probes = checks.iter().map(|c| Probe { check: c.clone(), ok: None, rtt_ms: None, last_ok: None, last_fail: None, down_since: None, tries: 0, fails: 0 }).collect();
        let p = Arc::new(Probes { probes: Mutex::new(probes), dns_ms: Mutex::new(None) });
        if checks.is_empty() {
            return p;
        }
        let p2 = p.clone();
        let _ = std::thread::Builder::new().name("net-probe".into()).spawn(move || {
            let mut next = vec![Instant::now(); checks.len()];
            loop {
                for (i, c) in checks.iter().enumerate() {
                    if Instant::now() < next[i] {
                        continue;
                    }
                    let (ok, rtt, dns) = c.run();
                    next[i] = Instant::now() + c.every(ok);
                    let internet = matches!(c, Check::Connect(_));
                    if let Some(d) = dns.filter(|_| internet) {
                        *p2.dns_ms.lock().unwrap() = Some(d);
                    }
                    let now = crate::clock::unix_s();
                    let mut v = p2.probes.lock().unwrap();
                    let pr = &mut v[i];
                    pr.tries += 1;
                    let was = pr.ok;
                    pr.ok = Some(ok);
                    pr.rtt_ms = rtt;
                    if ok {
                        pr.last_ok = Some(now);
                        if was == Some(false) {
                            let down = pr.down_since.take().map_or(0.0, |s| now - s);
                            if internet {
                                changed((c.target().to_string(), true, down));
                            }
                        }
                    } else {
                        pr.fails += 1;
                        pr.last_fail = Some(now);
                        if was != Some(false) {
                            pr.down_since = Some(now);
                            if was == Some(true) && internet {
                                changed((c.target().to_string(), false, 0.0));
                            }
                        }
                    }
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
        p
    }

    /// The internet checks into `sink` (`net/…`); each check's row (`kind`:
    /// "connect" or "time").
    fn report(&self, sink: &mut dyn Sink) -> Vec<Value> {
        let v = self.probes.lock().unwrap();
        let mut up = 0;
        let mut known = 0;
        let mut best: Option<f64> = None;
        let mut rows = Vec::new();
        for p in v.iter() {
            let internet = matches!(p.check, Check::Connect(_));
            if let (true, Some(ok)) = (internet, p.ok) {
                known += 1;
                up += ok as u32;
                if ok {
                    best = Some(best.map_or(p.rtt_ms.unwrap_or(0.0), |b: f64| b.min(p.rtt_ms.unwrap_or(b))));
                }
                let name = format!("net/probe/{}", key_part(p.check.target()));
                sink.gauge(&format!("{name}/up"), ok as u8 as f64);
                if let Some(r) = p.rtt_ms {
                    sink.gauge(&format!("{name}/rtt"), r);
                }
            }
            rows.push(json!({
                "kind": match p.check { Check::Connect(_) => "connect", Check::Time(_) => "time", Check::Service => "service" }, "target": p.check.target(), "ok": p.ok, "rttMs": p.rtt_ms.map(|r| (r * 10.0).round() / 10.0),
                "lastOk": p.last_ok, "lastFail": p.last_fail, "downSince": p.down_since, "tries": p.tries, "fails": p.fails,
            }));
        }
        if known > 0 {
            sink.gauge("net/up", up as f64 / known as f64);
        }
        if let Some(b) = best {
            sink.gauge("net/rtt", b);
        }
        if let Some(d) = *self.dns_ms.lock().unwrap() {
            sink.gauge("net/dns", d);
        }
        rows
    }
}

/// (answered, connect time ms, name lookup ms).
fn probe(target: &str) -> (bool, Option<f64>, Option<f64>) {
    let t = Instant::now();
    let addrs: Vec<_> = match target.to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(_) => return (false, None, None),
    };
    let named = target.rsplit_once(':').is_some_and(|(h, _)| h.parse::<std::net::IpAddr>().is_err() && !h.starts_with('['));
    let dns = named.then(|| t.elapsed().as_secs_f64() * 1000.0);
    for a in addrs.iter().take(2) {
        let t = Instant::now();
        if TcpStream::connect_timeout(a, PROBE_TIMEOUT).is_ok() {
            return (true, Some(t.elapsed().as_secs_f64() * 1000.0), dns);
        }
    }
    (false, None, dns)
}

/// A folder to watch the free space of: what it's for, and where.
pub struct Watched {
    pub name: &'static str,
    pub path: PathBuf,
}

pub struct Platform {
    sys: System,
    disks: Disks,
    nets: Networks,
    comps: Components,
    pid: Option<Pid>,
    watched: Vec<Watched>,
    pub probes: Arc<Probes>,
    container: Option<&'static str>,
    cpu_limit: Option<f64>,
    last_slow: Option<Instant>,
    disk_rows: Vec<Value>,
    temp: Option<f64>,
    net_rx: u64,
    net_tx: u64,
    net_err: u64,
}

impl Platform {
    pub fn new(watched: Vec<Watched>, probes: Arc<Probes>) -> Platform {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        Platform {
            sys,
            disks: Disks::new_with_refreshed_list(),
            nets: Networks::new_with_refreshed_list(),
            comps: Components::new_with_refreshed_list(),
            pid: sysinfo::get_current_pid().ok(),
            watched,
            probes,
            container: container(),
            cpu_limit: cpu_limit_cores(),
            last_slow: None,
            disk_rows: Vec::new(),
            temp: None,
            net_rx: 0,
            net_tx: 0,
            net_err: 0,
        }
    }

    /// The disk a path is on: the mount point that is its longest prefix.
    fn disk_of(&self, path: &Path) -> Option<&sysinfo::Disk> {
        let p = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.disks.list().iter().filter(|d| p.starts_with(d.mount_point())).max_by_key(|d| d.mount_point().as_os_str().len())
    }

    /// Sample everything into `sink` and return the `platform` message's
    /// body. `detail`: include per-core and per-interface figures.
    pub fn sample(&mut self, sink: &mut dyn Sink, detail: bool) -> Value {
        let mut unavailable: Vec<Value> = Vec::new();
        let mut missing = |field: &str, why: &str| unavailable.push(json!({ "field": field, "why": why }));
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        let ncpu = self.sys.cpus().len().max(1) as f64;
        // Inside a container with a CPU limit, the machine's CPU means little: share of the limit.
        let cap_cores = self.cpu_limit.map_or(ncpu, |l| l.min(ncpu));
        let total = self.sys.global_cpu_usage() as f64;
        sink.gauge("plat/cpu", total);
        if let Some(pid) = self.pid {
            self.sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), false, ProcessRefreshKind::nothing().with_cpu().with_memory());
            if let Some(p) = self.sys.process(pid) {
                let cores = p.cpu_usage() as f64 / 100.0;
                sink.gauge("plat/proCores", cores);
                sink.gauge("plat/proCpu", 100.0 * cores / cap_cores);
                sink.gauge("plat/proMem", p.memory() as f64);
            }
        } else {
            missing("proCpu", "this platform doesn't say which process we are");
        }
        let (mem_total, mem_avail) = match self.sys.cgroup_limits() {
            Some(c) if c.total_memory > 0 && c.total_memory < self.sys.total_memory() => (c.total_memory, c.free_memory),
            _ => (self.sys.total_memory(), self.sys.available_memory()),
        };
        if mem_total > 0 {
            sink.gauge("plat/mem", 100.0 * (1.0 - mem_avail as f64 / mem_total as f64));
            sink.gauge("plat/memAvail", mem_avail as f64);
        }
        if self.sys.total_swap() > 0 {
            sink.gauge("plat/swap", 100.0 * self.sys.used_swap() as f64 / self.sys.total_swap() as f64);
        }
        match (memory_psi(), memory_level()) {
            (Some(p), _) => sink.gauge("plat/memPsi", p),
            (None, Some(l)) => sink.gauge("plat/memLevel", l),
            (None, None) => missing("memPressure", "not reported on this platform"),
        }
        let la = System::load_average();
        if la.one > 0.0 {
            sink.gauge("plat/load1", la.one);
        } else if cfg!(windows) {
            missing("load", "Windows has no load average");
        }
        if let Some(t) = throttled_us() {
            sink.counter("plat/throttledUs", t);
        }
        if let Some(t) = pi_throttled() {
            sink.gauge("plat/piThrottled", t as f64);
        }
        // Network: every interface but loopback.
        self.nets.refresh(true);
        let (mut rx, mut tx, mut err) = (0u64, 0u64, 0u64);
        let mut ifaces = Vec::new();
        for (name, n) in self.nets.list() {
            if name == "lo" || name.starts_with("lo0") || name.starts_with("utun") || name.starts_with("awdl") || name.starts_with("llw") {
                continue;
            }
            rx += n.total_received();
            tx += n.total_transmitted();
            err += n.total_errors_on_received() + n.total_errors_on_transmitted();
            if detail && (n.total_received() > 0 || n.total_transmitted() > 0) {
                ifaces.push(json!({ "name": name, "rx": n.received(), "tx": n.transmitted(), "state": format!("{:?}", n.operational_state()) }));
            }
        }
        self.net_rx = self.net_rx.max(rx);
        self.net_tx = self.net_tx.max(tx);
        self.net_err = self.net_err.max(err);
        sink.counter("net/rx", rx);
        sink.counter("net/tx", tx);
        sink.counter("net/errors", err);
        let (probes, time): (Vec<Value>, Vec<Value>) = self.probes.report(sink).into_iter().partition(|r| r["kind"] == "connect");
        let clock = clock_report(sink, time.into_iter().find(|r| r["kind"] == "time"), &mut missing);
        // The slow ones.
        if self.last_slow.is_none_or(|t| t.elapsed() >= SLOW_EVERY) {
            self.last_slow = Some(Instant::now());
            self.disks.refresh(true);
            self.comps.refresh(true);
            self.disk_rows = self
                .watched
                .iter()
                .filter_map(|w| {
                    let d = self.disk_of(&w.path)?;
                    Some(json!({ "name": w.name, "path": w.path.display().to_string(), "mount": d.mount_point().display().to_string(), "totalBytes": d.total_space(), "freeBytes": d.available_space() }))
                })
                .collect();
            self.temp = self.comps.list().iter().filter_map(|c| c.temperature()).filter(|t| t.is_finite() && *t > 0.0).map(|t| t as f64).reduce(f64::max);
        }
        for d in &self.disk_rows {
            let (total, free) = (d["totalBytes"].as_f64().unwrap_or(0.0), d["freeBytes"].as_f64().unwrap_or(0.0));
            if total > 0.0 {
                let n = d["name"].as_str().unwrap_or("disk");
                sink.gauge(&format!("plat/disk/{n}/free"), free);
                sink.gauge(&format!("plat/disk/{n}/freePct"), 100.0 * free / total);
            }
        }
        if self.disk_rows.is_empty() {
            missing("disk", "couldn't find the recordings folder's disk");
        }
        match self.temp {
            Some(t) => sink.gauge("plat/temp", t),
            None => missing("temp", if self.container.is_some() { "no sensors inside the container" } else { "no temperature sensors reported" }),
        }
        json!({
            "os": System::long_os_version().or_else(System::name).unwrap_or_else(|| std::env::consts::OS.to_string()),
            "arch": System::cpu_arch(),
            "host": System::host_name(),
            "cores": ncpu as u32,
            "container": self.container,
            "cpuLimitCores": self.cpu_limit,
            "memTotal": mem_total,
            "uptimeS": System::uptime(),
            "disks": self.disk_rows,
            "probes": probes,
            "clock": clock,
            "perCore": if detail { Some(self.sys.cpus().iter().map(|c| c.cpu_usage().round()).collect::<Vec<_>>()) } else { None },
            "interfaces": if detail { Some(ifaces) } else { None },
            "unavailable": unavailable,
        })
    }
}

/// The clock (`clock/…` series, the `platform` message's `clock`): what the
/// OS says, read now, and the time server's last answer (`check`: its row).
fn clock_report(sink: &mut dyn Sink, check: Option<Value>, missing: &mut impl FnMut(&str, &str)) -> Value {
    let os = crate::clock::read_os();
    let h = crate::clock::health();
    let now = crate::clock::unix_s();
    match os {
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
    let radio_ref = h.radio_reference(now);
    if let Some((_, r)) = &radio_ref {
        sink.gauge("clock/radioMs", *r);
    }
    let round = |v: f64| (v * 10.0).round() / 10.0;
    json!({
        "warnMs": h.limits().0,
        "os": os.map(|o| json!({ "synced": o.synced, "maxErrorMs": round(o.max_error_ms) })),
        "service": h.service.as_ref().map(|s| json!({ "name": s.name, "server": s.server, "stratum": s.stratum, "lastSync": s.last_sync })),
        "driftMs": h.drift_ms.map(round),
        "radio": h.radio.iter().map(|r| json!({ "site": r.site, "offsetMs": round(r.offset_ms), "at": r.at.round(), "sane": r.sane(), "valid": r.valid })).collect::<Vec<_>>(),
        "server": h.server,
        "measured": h.measured.map(|(m, at)| json!({ "offsetMs": round(m.offset_ms), "delayMs": round(m.delay_ms), "stratum": m.stratum, "at": at, "current": current.is_some() })),
        "error": h.error,
        "problem": h.problem(now).map(|(bad, why)| json!({ "level": if bad { "bad" } else { "warn" }, "why": why })),
        "check": check,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cgroup_files() {
        assert_eq!(parse_cpu_max("150000 100000\n"), Some(1.5));
        assert_eq!(parse_cpu_max("max 100000\n"), None);
        assert_eq!(parse_psi("some avg10=1.25 avg60=0.50 avg300=0.10 total=123\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n"), Some(1.25));
        assert_eq!(container_from_cgroup("0::/system.slice/docker-abc.scope\n"), Some("docker"));
        assert_eq!(container_from_cgroup("0::/kubepods/burstable/pod1\n"), Some("kubernetes"));
        assert_eq!(container_from_cgroup("0::/init.scope\n"), None);
    }

    #[derive(Default)]
    struct Keys(Vec<String>);
    impl Sink for Keys {
        fn counter(&mut self, n: &str, _: u64) {
            self.0.push(n.into());
        }
        fn gauge(&mut self, n: &str, _: f64) {
            self.0.push(n.into());
        }
    }

    #[test]
    fn samples_this_machine() {
        let mut p = Platform::new(vec![Watched { name: "data", path: std::env::temp_dir() }], Arc::new(Probes::default()));
        let mut k = Keys::default();
        let v = p.sample(&mut k, true);
        assert!(k.0.contains(&"plat/cpu".to_string()));
        assert!(k.0.contains(&"plat/mem".to_string()));
        assert!(v["cores"].as_u64().unwrap() >= 1);
        assert!(v["perCore"].is_array());
    }
}
