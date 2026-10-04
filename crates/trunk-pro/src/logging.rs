//! The log's routing: every line goes, as the config's `log` section says,
//! to the console (stderr, so it can be redirected on its own), to files,
//! and to the system log. What a line says is the session's
//! ([`trunk_app::log::Record`]) or a plain message from anywhere in the app
//! through the `log` crate (`log::info!`, …); how it reads is
//! [`trunk_app::log::full_line`]'s. Nothing here knows what is logged.
//!
//! Files are Trunk Recorder's: `<dir>/%m-%d-%Y_%H%M_<n>.log`, a new one each
//! day and at 100 MB; with `syslogFriendly`, one `<dir>/trunk-pro.log`
//! appended to, for logrotate (SIGHUP reopens it).

use std::fs::{File, OpenOptions};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, RwLock};

use chrono::{Datelike, Local};
use trunk_app::config::LogSettings;
use trunk_app::log::{full_line, Format, Level, Record};

const ROTATE_BYTES: u64 = 100 * 1024 * 1024;

struct Settings {
    level: Level,
    console: bool,
    console_color: bool,
    file: bool,
    file_color: bool,
    syslog: bool,
    format: Format,
}

struct FileSink {
    dir: PathBuf,
    /// One file for logrotate (syslogFriendly), else daily / 100 MB.
    single: bool,
    file: Option<File>,
    written: u64,
    /// The day the file was opened (yyyymmdd).
    day: u32,
}

pub struct Logger {
    settings: RwLock<Settings>,
    file: Mutex<Option<FileSink>>,
    syslog: Mutex<Option<Syslog>>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

fn logger() -> &'static Logger {
    LOGGER.get_or_init(|| Logger {
        settings: RwLock::new(Settings {
            level: Level::Info,
            console: true,
            console_color: false,
            file: false,
            file_color: false,
            syslog: false,
            format: Format::default(),
        }),
        file: Mutex::new(None),
        syslog: Mutex::new(None),
    })
}

/// `--log-level`: wins over the config's.
static LEVEL_OVERRIDE: Mutex<Option<Level>> = Mutex::new(None);

/// Start logging with `s` (`config_dir`: where a relative `dir` is) and take
/// the `log` crate's messages; `level`: `--log-level`, over the config's.
pub fn init(s: &LogSettings, config_dir: &Path, level: Option<Level>) {
    *LEVEL_OVERRIDE.lock().unwrap() = level;
    configure(s, config_dir);
}

/// The settings changed (the config was saved).
pub fn configure(s: &LogSettings, config_dir: &Path) {
    let l = logger();
    let level_override = *LEVEL_OVERRIDE.lock().unwrap();
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    // Trunk Recorder's logColor (its default "console", "none" under NO_COLOR); unset: colour only on a terminal.
    let color = match s.color.trim() {
        "" if no_color => "none",
        "" if std::io::stderr().is_terminal() => "console",
        "" => "none",
        c => c,
    };
    let dir = if s.dir.trim().is_empty() { config_dir.join("logs") } else { config_dir.join(s.dir.trim()) };
    let level = level_override.unwrap_or(s.level);
    *l.settings.write().unwrap() = Settings {
        level,
        console: s.console,
        console_color: matches!(color, "console" | "all"),
        file: s.file,
        file_color: matches!(color, "logfile" | "all"),
        syslog: s.syslog,
        format: s.format(),
    };
    {
        let mut f = l.file.lock().unwrap();
        let keep = f.as_ref().is_some_and(|f| f.dir == dir && f.single == s.syslog_friendly);
        if !s.file {
            *f = None;
        } else if !keep {
            *f = Some(FileSink { dir, single: s.syslog_friendly, file: None, written: 0, day: 0 });
        }
    }
    {
        let mut y = l.syslog.lock().unwrap();
        if !s.syslog {
            *y = None;
        } else if y.is_none() {
            *y = Syslog::connect();
            if y.is_none() {
                drop(y);
                record(&Record::text(Level::Warning, None, "No system log on this platform: log to a file instead"));
            }
        }
    }
    let _ = log::set_logger(l);
    log::set_max_level(match level {
        Level::Trace => log::LevelFilter::Trace,
        Level::Debug => log::LevelFilter::Debug,
        Level::Info => log::LevelFilter::Info,
        Level::Warning => log::LevelFilter::Warn,
        Level::Error | Level::Fatal => log::LevelFilter::Error,
    });
}

/// Reopen the log file (SIGHUP, after logrotate moved it).
pub fn reopen() {
    if let Some(f) = logger().file.lock().unwrap().as_mut() {
        f.file = None;
    }
}

/// Log a record from the session (or anywhere).
pub fn record(r: &Record) {
    let l = logger();
    let s = l.settings.read().unwrap();
    if r.level < s.level {
        return;
    }
    let time = Local::now().format("%Y-%m-%d %H:%M:%S%.6f").to_string();
    if s.console {
        let _ = writeln!(std::io::stderr().lock(), "{}", full_line(&time, r, &s.format, s.console_color));
    }
    if s.file {
        if let Some(f) = l.file.lock().unwrap().as_mut() {
            f.write(&full_line(&time, r, &s.format, s.file_color));
        }
    }
    if s.syslog {
        if let Some(y) = l.syslog.lock().unwrap().as_ref() {
            // The system log has its own time and severity.
            y.send(r.level, &trunk_app::log::line(r, &s.format, false));
        }
    }
}

/// A plain message (what `log::info!` and friends make).
pub fn text(level: Level, text: impl Into<String>) {
    record(&Record::text(level, None, text));
}

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        level_of(m.level()) >= self.settings.read().unwrap().level
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            text(level_of(r.level()), r.args().to_string());
        }
    }
    fn flush(&self) {}
}

fn level_of(l: log::Level) -> Level {
    match l {
        log::Level::Trace => Level::Trace,
        log::Level::Debug => Level::Debug,
        log::Level::Info => Level::Info,
        log::Level::Warn => Level::Warning,
        log::Level::Error => Level::Error,
    }
}

impl FileSink {
    fn write(&mut self, line: &str) {
        let now = Local::now();
        let day = now.year() as u32 * 10000 + now.month() * 100 + now.day();
        let rotate = !self.single && (day != self.day || self.written >= ROTATE_BYTES);
        if self.file.is_none() || rotate {
            self.open(&now, day);
        }
        if let Some(f) = self.file.as_mut() {
            if writeln!(f, "{line}").is_ok() {
                self.written += line.len() as u64 + 1;
            }
        }
    }

    fn open(&mut self, now: &chrono::DateTime<Local>, day: u32) {
        self.file = None;
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let path = if self.single {
            self.dir.join("trunk-pro.log")
        } else {
            // Trunk Recorder's name: %m-%d-%Y_%H%M_%2N.log, N counting up.
            let stem = now.format("%m-%d-%Y_%H%M").to_string();
            (0..100).map(|n| self.dir.join(format!("{stem}_{n:02}.log"))).find(|p| !p.exists()).unwrap_or_else(|| self.dir.join(format!("{stem}_99.log")))
        };
        self.file = OpenOptions::new().create(true).append(true).open(&path).ok();
        self.written = self.file.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());
        self.day = day;
    }
}

/// The system log, through the C library's syslog(3): journald / rsyslog
/// on Linux, the unified log on macOS (`log show --predicate 'process == "trunk-pro"'`).
struct Syslog;

#[cfg(unix)]
mod libc_syslog {
    use std::os::raw::{c_char, c_int};
    unsafe extern "C" {
        pub fn openlog(ident: *const c_char, option: c_int, facility: c_int);
        pub fn syslog(priority: c_int, format: *const c_char, ...);
    }
    /// LOG_PID; LOG_USER.
    pub const LOG_PID: c_int = 0x01;
    pub const LOG_USER: c_int = 1 << 3;
}

impl Syslog {
    #[cfg(unix)]
    fn connect() -> Option<Syslog> {
        // SAFETY: openlog keeps the pointer: a static string.
        unsafe { libc_syslog::openlog(c"trunk-pro".as_ptr(), libc_syslog::LOG_PID, libc_syslog::LOG_USER) };
        Some(Syslog)
    }
    #[cfg(not(unix))]
    fn connect() -> Option<Syslog> {
        None
    }

    #[cfg(unix)]
    fn send(&self, level: Level, line: &str) {
        let Ok(msg) = std::ffi::CString::new(line.replace('\0', "")) else { return };
        // SAFETY: a "%s" format and one C string.
        unsafe { libc_syslog::syslog(level.syslog_severity() as std::os::raw::c_int | libc_syslog::LOG_USER, c"%s".as_ptr(), msg.as_ptr()) };
    }
    #[cfg(not(unix))]
    fn send(&self, _: Level, _: &str) {}
}
