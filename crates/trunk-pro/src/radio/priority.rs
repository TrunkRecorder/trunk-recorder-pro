//! Real-time scheduling for the threads that carry a radio's samples.
//!
//! A radio's driver keeps only a short buffer (65–440 ms); the threads that
//! empty it — ours, and the ones the driver starts (libusb's event thread on
//! macOS, nusb's, UHD's and libairspy's workers) — are lost when they wait
//! behind other work. [`crate::sdr::run_with`] raises a source's own thread
//! once and, each time the radio opens, the driver's threads started meanwhile.
//!
//! - **macOS**: QoS isn't inherited by threads a library starts, and a burst
//!   of process launches (the hourly contacts sync) kept them waiting long
//!   enough to overflow a B200 at 8 MSPS. So these threads get Mach's
//!   time-constraint policy, as Core Audio's I/O threads do: they run ahead
//!   of everything else for the few hundred µs each wakeup needs, and the
//!   kernel demotes one that overruns.
//! - **Linux**: SCHED_RR at three quarters of its range (sdr-core's choice,
//!   what UHD asks for). Nothing demotes a SCHED_RR thread that spins, so
//!   only threads known to wait on USB are raised: the source's own and
//!   nusb's event thread. Raising needs CAP_SYS_NICE (or an RLIMIT_RTPRIO);
//!   refused, it says so once — a Raspberry Pi 5 shed ~46 s of samples over
//!   12 h at normal priority (sdr-core's measurement).

/// The threads running before a radio opened, so [`raise_new`] can tell the ones its driver started.
pub struct Before(#[allow(dead_code)] Vec<u64>);

/// Driver threads with names of their own: libusb's on macOS (UHD, libairspy,
/// most SoapySDR modules) and nusb's (RTL-SDR, HackRF).
#[allow(dead_code)]
const NAMED: &[&str] = &["org.libusb.device-hotplug", "nusb-events"];

#[cfg(target_os = "macos")]
// libc's Mach bindings point at the mach2 crate; these few are enough.
#[allow(deprecated)]
mod os {
    use std::ffi::{CStr, c_char, c_int};

    use libc::{mach_port_t, thread_act_t};

    unsafe extern "C" {
        fn mach_port_deallocate(task: mach_port_t, name: mach_port_t) -> c_int;
    }

    /// Each thread of this process: its unique id, name, and port (a send right to release).
    fn threads() -> Vec<(u64, String, thread_act_t)> {
        let mut list: libc::thread_act_array_t = std::ptr::null_mut();
        let mut n: libc::mach_msg_type_number_t = 0;
        // SAFETY: task_threads fills a vm-allocated array of n thread ports,
        // each released below and the array deallocated.
        unsafe {
            if libc::task_threads(libc::mach_task_self(), &mut list, &mut n) != 0 {
                return Vec::new();
            }
            let ports = std::slice::from_raw_parts(list, n as usize).to_vec();
            libc::vm_deallocate(libc::mach_task_self(), list as libc::vm_address_t, n as usize * size_of::<thread_act_t>());
            ports
                .into_iter()
                .filter_map(|port| {
                    let mut info: libc::thread_identifier_info_data_t = std::mem::zeroed();
                    let mut count = libc::THREAD_IDENTIFIER_INFO_COUNT;
                    if libc::thread_info(port, libc::THREAD_IDENTIFIER_INFO as u32, &mut info as *mut _ as libc::thread_info_t, &mut count) != 0 {
                        mach_port_deallocate(libc::mach_task_self(), port);
                        return None;
                    }
                    let mut buf = [0 as c_char; 64];
                    let p = libc::pthread_from_mach_thread_np(port);
                    if p != 0 {
                        libc::pthread_getname_np(p, buf.as_mut_ptr(), buf.len());
                    }
                    let name = CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned();
                    Some((info.thread_id, name, port))
                })
                .collect()
        }
    }

    fn release(threads: Vec<(u64, String, thread_act_t)>) {
        for (_, _, port) in threads {
            // SAFETY: a send right task_threads gave us.
            unsafe { mach_port_deallocate(libc::mach_task_self(), port) };
        }
    }

    /// Up to 2 ms of CPU per wakeup, done within 10 ms; not periodic.
    fn realtime(port: thread_act_t) -> bool {
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: plain Mach calls on a thread of this task.
        unsafe {
            libc::mach_timebase_info(&mut tb);
            let ms = 1e6 * tb.denom as f64 / tb.numer as f64;
            let mut p = libc::thread_time_constraint_policy_data_t { period: 0, computation: (2.0 * ms) as u32, constraint: (10.0 * ms) as u32, preemptible: 1 };
            libc::thread_policy_set(port, libc::THREAD_TIME_CONSTRAINT_POLICY as u32, &mut p as *mut _ as libc::thread_policy_t, libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT) == 0
        }
    }

    pub fn before() -> Vec<u64> {
        let t = threads();
        let ids = t.iter().map(|(id, ..)| *id).collect();
        release(t);
        ids
    }

    pub fn raise_self() -> Result<(), String> {
        // SAFETY: this thread's own port (released after).
        unsafe {
            let port = libc::mach_thread_self();
            let ok = realtime(port);
            mach_port_deallocate(libc::mach_task_self(), port);
            if ok { Ok(()) } else { Err("the time-constraint policy was refused".into()) }
        }
    }

    /// The driver's: started since `before` and unnamed (all of ours have
    /// names), or one of the [`super::NAMED`] USB event threads.
    pub fn raise_new(before: &[u64]) {
        let t = threads();
        for (id, name, port) in &t {
            let driver = (name.is_empty() && !before.contains(id)) || super::NAMED.contains(&name.as_str());
            if driver && !realtime(*port) {
                log::debug!("couldn't make driver thread {id} {name:?} real time");
            }
        }
        release(t);
    }
}

#[cfg(target_os = "linux")]
mod os {
    /// SCHED_RR at three quarters of its range.
    fn param() -> libc::sched_param {
        // SAFETY: plain queries.
        let (min, max) = unsafe { (libc::sched_get_priority_min(libc::SCHED_RR), libc::sched_get_priority_max(libc::SCHED_RR)) };
        libc::sched_param { sched_priority: min + (max - min) * 3 / 4 }
    }

    pub fn before() -> Vec<u64> {
        Vec::new()
    }

    pub fn raise_self() -> Result<(), String> {
        // SAFETY: this thread's own scheduling.
        let rc = unsafe { libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_RR, &param()) };
        if rc == 0 { Ok(()) } else { Err(std::io::Error::from_raw_os_error(rc).to_string()) }
    }

    /// nusb's event thread (by name: unnamed threads aren't known to wait).
    pub fn raise_new(_before: &[u64]) {
        let Ok(dir) = std::fs::read_dir("/proc/self/task") else { return };
        for t in dir.flatten() {
            let Some(tid) = t.file_name().to_str().and_then(|s| s.parse::<libc::pid_t>().ok()) else { continue };
            let name = std::fs::read_to_string(t.path().join("comm")).unwrap_or_default();
            if super::NAMED.contains(&name.trim()) {
                // SAFETY: a thread of this process.
                if unsafe { libc::sched_setscheduler(tid, libc::SCHED_RR, &param()) } != 0 {
                    log::debug!("couldn't make driver thread {tid} {name:?} real time");
                }
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod os {
    pub fn before() -> Vec<u64> {
        Vec::new()
    }
    pub fn raise_self() -> Result<(), String> {
        Ok(())
    }
    pub fn raise_new(_before: &[u64]) {}
}

/// Note the threads running now; call before opening a device.
pub fn before() -> Before {
    Before(os::before())
}

/// Make the calling thread (a source's receive thread) real time. A refusal
/// is logged once per run, with the fix.
pub fn raise_self() {
    static TOLD: std::sync::Once = std::sync::Once::new();
    if let Err(e) = os::raise_self() {
        TOLD.call_once(|| {
            let fix = if cfg!(target_os = "linux") {
                " Allow it with `sudo setcap cap_sys_nice+ep $(which trunk-pro)` (again after each update), or AmbientCapabilities=CAP_SYS_NICE in a system service's unit (a user service can't grant it)."
            } else {
                ""
            };
            log::warn!("Radios' receive threads can't run at real-time priority ({e}): under load they can lose samples.{fix}");
        });
    }
}

/// Make the threads the driver started since `before` real time, and the
/// USB event threads. Call once streaming (the driver's threads exist).
pub fn raise_new(before: &Before) {
    os::raise_new(&before.0);
}

#[cfg(all(test, target_os = "macos"))]
#[allow(deprecated)]
mod tests {
    use std::sync::mpsc;

    /// Whether the calling thread runs under the time-constraint policy.
    fn is_realtime() -> bool {
        // SAFETY: Mach calls on this thread's own port.
        unsafe {
            let port = libc::mach_thread_self();
            let mut p: libc::thread_time_constraint_policy_data_t = std::mem::zeroed();
            let mut count = libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT;
            let mut default = 0;
            libc::thread_policy_get(port, libc::THREAD_TIME_CONSTRAINT_POLICY as u32, &mut p as *mut _ as libc::thread_policy_t, &mut count, &mut default);
            default == 0
        }
    }

    /// A thread that reports, when asked, whether it's real time.
    fn spawn(name: Option<&str>) -> (mpsc::Sender<()>, mpsc::Receiver<bool>) {
        let (ask, asked) = mpsc::channel::<()>();
        let (answer, answers) = mpsc::channel();
        let f = move || {
            while asked.recv().is_ok() {
                answer.send(is_realtime()).unwrap();
            }
        };
        match name {
            Some(n) => drop(std::thread::Builder::new().name(n.into()).spawn(f).unwrap()),
            None => drop(std::thread::spawn(f)),
        }
        (ask, answers)
    }

    #[test]
    fn raises_the_drivers_threads_only() {
        let (old_ask, old) = spawn(None);
        let b = super::before();
        let (new_ask, new) = spawn(None);
        let (ours_ask, ours) = spawn(Some("engine"));
        let ask = |a: &mpsc::Sender<()>, r: &mpsc::Receiver<bool>| {
            a.send(()).unwrap();
            r.recv().unwrap()
        };
        assert!(!ask(&new_ask, &new));
        super::raise_new(&b);
        assert!(ask(&new_ask, &new), "an unnamed thread started since before()");
        assert!(!ask(&old_ask, &old), "one already running");
        assert!(!ask(&ours_ask, &ours), "one of ours (named)");
    }
}
